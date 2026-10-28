//! One interface per session (ruling P35). The first launch owns `com.cellglide.Gui` on the
//! session bus; a later launch asks it to open its main window and exits, or, started with
//! `--background` (autostart), just exits. Without a session bus every launch runs normally.
// The decision and the activation stream compile everywhere for tests; only Linux claims the name.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use crate::app::Message;
use futures::Stream;
use std::sync::{Mutex, PoisonError};
use tokio::sync::mpsc::Receiver;

pub const BUS_NAME: &str = "com.cellglide.Gui";
pub const OBJECT_PATH: &str = "/com/cellglide/Gui";

/// How claiming the bus name went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bus {
    /// This process owns the name: it is the instance.
    Owned,
    /// Another process owns it.
    Taken,
    /// No session bus (or it failed): nothing to coordinate with.
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Launch {
    Run,
    Exit,
}

/// Whether this launch runs. When another instance owns the name, a manual launch asks it to
/// open its window (`activate` says whether that worked) and exits; if it didn't answer, this
/// launch runs after all rather than leave the user with nothing.
pub fn decide(bus: Bus, background: bool, activate: impl FnOnce() -> bool) -> Launch {
    match bus {
        Bus::Owned | Bus::Unavailable => Launch::Run,
        Bus::Taken if background => Launch::Exit,
        Bus::Taken if activate() => Launch::Exit,
        Bus::Taken => Launch::Run,
    }
}

/// `Activate` calls waiting for the app; more than this many add nothing.
const PENDING: usize = 4;

/// Where `Activate` calls wait until the app's subscription takes them.
static ACTIVATIONS: Mutex<Option<Receiver<()>>> = Mutex::new(None);

/// Hands the receiving end of the `Activate` calls to [`activations`]; only the process that
/// owns the name keeps one.
fn keep(rx: Receiver<()>) {
    *ACTIVATIONS.lock().unwrap_or_else(PoisonError::into_inner) = Some(rx);
}

/// Each `Activate` call as a request to open the main window. Ends at once when this process
/// doesn't own the name.
pub fn activations() -> impl Stream<Item = Message> {
    let rx = ACTIVATIONS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    futures::stream::unfold(rx, |rx| async move {
        let mut rx = rx?;
        rx.recv()
            .await
            .map(|()| (Message::OpenWindow(None), Some(rx)))
    })
}

#[cfg(target_os = "linux")]
pub use linux::start;

#[cfg(target_os = "linux")]
mod linux {
    use super::{BUS_NAME, Bus, Launch, OBJECT_PATH, PENDING, decide, keep};
    use std::time::Duration;
    use tokio::sync::mpsc::{Sender, channel};
    use zbus::fdo::RequestNameFlags;

    /// How long a second launch waits for the running instance to answer `Activate`.
    const ACTIVATE_TIMEOUT: Duration = Duration::from_secs(5);

    struct Gui {
        tx: Sender<()>,
    }

    #[zbus::interface(name = "com.cellglide.Gui")]
    impl Gui {
        /// Opens (or raises) the main window.
        fn activate(&self) {
            // Already full: the window is about to open anyway.
            let _ = self.tx.try_send(());
        }
    }

    /// Keeps the name owned while the app runs: the connection and the runtime its tasks run on.
    pub struct Instance {
        conn: Option<zbus::Connection>,
        runtime: Option<tokio::runtime::Runtime>,
    }

    impl Instance {
        /// Whether this process owns the bus name (false without a session bus).
        #[cfg(test)]
        pub fn owns_name(&self) -> bool {
            self.conn.is_some()
        }
    }

    impl Drop for Instance {
        fn drop(&mut self) {
            // The connection's tasks belong to this runtime; close it inside it.
            if let (Some(runtime), Some(conn)) = (&self.runtime, self.conn.take()) {
                let _inside = runtime.enter();
                drop(conn);
            }
        }
    }

    /// Serves `Activate`, then asks for the name without queueing: `NameTaken` when another
    /// process has it. (zbus's connection builder requests names without `DoNotQueue`, so a
    /// second launch would wait in the queue and inherit the name; hence the explicit request.)
    async fn claim(tx: Sender<()>) -> zbus::Result<zbus::Connection> {
        let conn = zbus::Connection::session().await?;
        conn.object_server().at(OBJECT_PATH, Gui { tx }).await?;
        conn.request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into())
            .await?;
        Ok(conn)
    }

    async fn activate_running() -> bool {
        let call = async {
            let conn = zbus::Connection::session().await?;
            conn.call_method(Some(BUS_NAME), OBJECT_PATH, Some(BUS_NAME), "Activate", &())
                .await
                .map(drop)
        };
        match tokio::time::timeout(ACTIVATE_TIMEOUT, call).await {
            Ok(Ok(())) => true,
            Ok(Err(error)) => {
                eprintln!("cellglide-gui: the running instance didn't answer: {error}");
                false
            }
            Err(_) => {
                eprintln!("cellglide-gui: the running instance didn't answer in time");
                false
            }
        }
    }

    /// Claims the name, or hands over to the instance that has it. `None`: exit now.
    pub fn start(background: bool) -> Option<Instance> {
        let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("cellglide-bus")
            .enable_all()
            .build()
        else {
            return Some(Instance {
                conn: None,
                runtime: None,
            });
        };
        let (tx, rx) = channel(PENDING);
        let (bus, conn) = match runtime.block_on(claim(tx)) {
            Ok(conn) => {
                keep(rx);
                (Bus::Owned, Some(conn))
            }
            Err(zbus::Error::NameTaken) => (Bus::Taken, None),
            Err(error) => {
                eprintln!(
                    "cellglide-gui: no session bus, not checking for another instance: {error}"
                );
                (Bus::Unavailable, None)
            }
        };
        match decide(bus, background, || runtime.block_on(activate_running())) {
            Launch::Exit => None,
            Launch::Run => Some(Instance {
                conn,
                runtime: Some(runtime),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::cell::Cell;
    use tokio::sync::mpsc::channel;

    #[test]
    fn first_launch_and_no_bus_run_without_activating() {
        for bus in [Bus::Owned, Bus::Unavailable] {
            for background in [false, true] {
                let asked = Cell::new(false);
                let launch = decide(bus, background, || {
                    asked.set(true);
                    true
                });
                assert_eq!(launch, Launch::Run, "{bus:?} background={background}");
                assert!(!asked.get());
            }
        }
    }

    #[test]
    fn second_background_launch_exits_quietly() {
        let asked = Cell::new(false);
        let launch = decide(Bus::Taken, true, || {
            asked.set(true);
            true
        });
        assert_eq!(launch, Launch::Exit);
        assert!(!asked.get(), "autostart never opens the other window");
    }

    #[test]
    fn second_manual_launch_activates_the_running_one() {
        assert_eq!(decide(Bus::Taken, false, || true), Launch::Exit);
        // The running instance didn't answer: run rather than show nothing.
        assert_eq!(decide(Bus::Taken, false, || false), Launch::Run);
    }

    #[tokio::test]
    async fn activate_calls_open_the_main_window() {
        let (tx, rx) = channel(PENDING);
        keep(rx);
        tx.try_send(()).unwrap();
        tx.try_send(()).unwrap();
        drop(tx);
        let messages: Vec<_> = activations().collect().await;
        assert_eq!(messages.len(), 2);
        assert!(
            messages
                .iter()
                .all(|m| matches!(m, Message::OpenWindow(None)))
        );
        // Taken once: a second subscription (not this process's name) yields nothing.
        assert_eq!(activations().count().await, 0);
    }

    /// Against a private session bus: `dbus-run-session -- cargo test -p cg-gui single:: --
    /// --include-ignored --test-threads=1` (one thread: the activation queue is shared with the
    /// test above). Ignored by default because a running CellGlide would own the name.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "needs a private session bus"]
    fn real_bus_second_launch_activates_the_first() {
        let first = start(false).expect("the first launch runs");
        assert!(first.owns_name());
        // Autostart while the first runs: exits without opening anything.
        assert!(start(true).is_none());
        // A manual launch: the first gets Activate, the second exits.
        assert!(start(false).is_none());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let first_message = runtime.block_on(async {
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                std::pin::pin!(activations()).next(),
            )
            .await
        });
        assert!(matches!(first_message, Ok(Some(Message::OpenWindow(None)))));
        drop(first);
    }
}
