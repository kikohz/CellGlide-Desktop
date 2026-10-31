//! The tray icon and the service's snapshots, as one stream of app messages.
// The tray itself is Linux-only; the rest compiles on any Unix for tests.
#![cfg_attr(not(target_os = "linux"), allow(dead_code, unused_imports))]

use crate::app::Message;
#[cfg(target_os = "linux")]
use cg_i18n::tr;
use cg_ipc::Endpoint;
use cg_ipc::client::{self, Update};
use cg_ipc::describe::ui::{MenuEntry, TrayLook, down_look, menu_layout, tray_look, version_line};
use futures::channel::mpsc::Sender;
use futures::{SinkExt, Stream};
use std::time::Duration;
#[cfg(target_os = "linux")]
use std::{
    sync::{Arc, Mutex, PoisonError},
    time::Instant,
};

/// Hands the tray a new look, waking it only when the look changed.
fn show(state: &tokio::sync::watch::Sender<TrayLook>, look: TrayLook) {
    state.send_if_modified(|current| {
        let changed = *current != look;
        if changed {
            *current = look;
        }
        changed
    });
}

/// The themed icon for a state; the files live in packaging/icons/tray.
pub fn icon_name(active: bool, alert: bool) -> &'static str {
    match (active, alert) {
        (false, false) => "cellglide-tray-symbolic",
        (true, false) => "cellglide-tray-active-symbolic",
        (false, true) => "cellglide-tray-alert-symbolic",
        (true, true) => "cellglide-tray-active-alert-symbolic",
    }
}

#[cfg(target_os = "linux")]
struct Tray {
    tx: Sender<Message>,
    look: TrayLook,
    /// True while no StatusNotifierWatcher is on the bus. Held while the matching `TrayReady`
    /// is queued, so the app sees the messages in the order the flag changed.
    offline: Arc<Mutex<bool>>,
}

#[cfg(target_os = "linux")]
impl Tray {
    fn report(&self, offline: bool) {
        let mut flag = self.offline.lock().unwrap_or_else(PoisonError::into_inner);
        *flag = offline;
        let _ = self.tx.clone().try_send(Message::TrayReady(!offline));
    }
}

#[cfg(target_os = "linux")]
impl ksni::Tray for Tray {
    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        self.report(true);
        true
    }
    fn watcher_online(&self) {
        self.report(false);
    }
    fn id(&self) -> String {
        "cellglide".into()
    }
    fn title(&self) -> String {
        "CellGlide".into()
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: version_line(),
            description: self.look.title.clone(),
            ..Default::default()
        }
    }
    fn icon_name(&self) -> String {
        icon_name(self.look.active, self.look.alert).into()
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.clone().try_send(Message::TogglePanel);
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        menu_layout(&self.look)
            .into_iter()
            .map(|(entry, enabled)| {
                let (label, message) = match entry {
                    MenuEntry::Separator => return ksni::MenuItem::Separator,
                    MenuEntry::Version => (version_line(), None),
                    MenuEntry::Toggle { disconnect } => (
                        tr(
                            if disconnect {
                                "tray.disconnect"
                            } else {
                                "tray.connect"
                            },
                            &[],
                        ),
                        Some(Message::Toggle),
                    ),
                    MenuEntry::Open => (tr("tray.open", &[]), Some(Message::OpenWindow(None))),
                    MenuEntry::Quit => (tr("tray.quit", &[]), Some(Message::Quit)),
                };
                StandardItem {
                    label,
                    enabled,
                    activate: Box::new(move |t: &mut Self| {
                        if let Some(message) = message.clone() {
                            let _ = t.tx.clone().try_send(message);
                        }
                    }),
                    ..Default::default()
                }
                .into()
            })
            .collect()
    }
}

#[cfg(target_os = "linux")]
const WATCHER: &str = "org.kde.StatusNotifierWatcher";
/// Respawns of the tray when the watcher is on the bus but never registered us.
#[cfg(target_os = "linux")]
const MAX_RESPAWNS: u32 = 5;

#[cfg(target_os = "linux")]
async fn watcher_present(conn: &mut Option<zbus::Connection>) -> bool {
    if conn.is_none() {
        *conn = zbus::Connection::session().await.ok();
    }
    let Some(conn) = conn else { return false };
    let Ok(proxy) = zbus::fdo::DBusProxy::new(conn).await else {
        return false;
    };
    let Ok(name) = zbus::names::BusName::try_from(WATCHER) else {
        return false;
    };
    proxy.name_has_owner(name).await.unwrap_or(false)
}

#[cfg(target_os = "linux")]
async fn spawn_tray(
    tx: &Sender<Message>,
    offline: &Arc<Mutex<bool>>,
    look: TrayLook,
) -> Option<ksni::Handle<Tray>> {
    use ksni::TrayMethods;
    // The tray host (GNOME's AppIndicator extension) may start after us at login, so a
    // missing watcher is not fatal: the item registers once it appears.
    *offline.lock().unwrap_or_else(PoisonError::into_inner) = false;
    let spawned = Tray {
        tx: tx.clone(),
        look,
        offline: offline.clone(),
    }
    .assume_sni_available(true)
    .spawn()
    .await
    .ok();
    let mut out = tx.clone();
    // A missing watcher was already reported by `watcher_offline` while spawning.
    let flag = offline.lock().unwrap_or_else(PoisonError::into_inner);
    if spawned.is_none() || !*flag {
        let _ = out.try_send(Message::TrayReady(spawned.is_some()));
    }
    spawned
}

/// Owns the tray: applies state changes, and respawns it if the watcher is on the bus but
/// ksni missed it (it can lose a race between its failed registration and its subscription).
#[cfg(target_os = "linux")]
async fn run_tray(tx: Sender<Message>, mut state: tokio::sync::watch::Receiver<TrayLook>) {
    let offline = Arc::new(Mutex::new(false));
    let initial = state.borrow().clone();
    let Some(mut handle) = spawn_tray(&tx, &offline, initial).await else {
        return;
    };
    let mut bus = None;
    let mut stuck_since: Option<Instant> = None;
    let mut respawns = 0;
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            changed = state.changed() => {
                if changed.is_err() {
                    return;
                }
                let look = state.borrow_and_update().clone();
                handle.update(move |t: &mut Tray| t.look = look).await;
            }
            _ = tick.tick() => {
                let is_offline = *offline.lock().unwrap_or_else(PoisonError::into_inner);
                if !is_offline || respawns >= MAX_RESPAWNS || !watcher_present(&mut bus).await {
                    stuck_since = None;
                    continue;
                }
                let since = *stuck_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_secs(3) {
                    stuck_since = None;
                    respawns += 1;
                    handle.shutdown().await;
                    let current = state.borrow().clone();
                    match spawn_tray(&tx, &offline, current).await {
                        Some(new) => handle = new,
                        None => return,
                    }
                }
            }
        }
    }
}

/// How long to wait before subscribing again after the service went away.
const RETRY: Duration = Duration::from_secs(2);

pub fn events() -> impl Stream<Item = Message> {
    iced::stream::channel(32, |mut output: Sender<Message>| async move {
        #[cfg(target_os = "linux")]
        let tray_state = {
            let (state_tx, state_rx) = tokio::sync::watch::channel(tray_look(None));
            tokio::spawn(run_tray(output.clone(), state_rx));
            state_tx
        };
        let mut updates = client::watch(Endpoint::system(), RETRY);
        while let Some(update) = updates.recv().await {
            match update {
                Update::Snapshot(snapshot) => {
                    #[cfg(target_os = "linux")]
                    show(&tray_state, tray_look(Some(&snapshot)));
                    if output.send(Message::Snapshot(snapshot)).await.is_err() {
                        return;
                    }
                }
                Update::Down(down) => {
                    // The last snapshot no longer holds: the tray stops saying "connected".
                    #[cfg(target_os = "linux")]
                    show(&tray_state, down_look(&down));
                    let _ = output.send(Message::ServiceDown(down)).await;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cg_ipc::describe::ui::ServiceDown;
    use cg_ipc::health::{HealthStatus, IssueCause};
    use cg_ipc::{ConnState, DnsMode, Snapshot};
    use std::path::Path;

    #[test]
    fn four_icon_names_match_the_packaged_files() {
        let names = [
            icon_name(false, false),
            icon_name(true, false),
            icon_name(false, true),
            icon_name(true, true),
        ];
        for (i, name) in names.iter().enumerate() {
            assert!(!names[..i].contains(name), "{name} repeats");
            let file = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../packaging/icons/tray")
                .join(format!("{name}.svg"));
            let svg = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{file:?}: {e}"));
            assert!(!svg.to_lowercase().contains("#bebebe"), "{name}");
            assert!(svg.contains("viewBox=\"0 0 16 16\""), "{name}");
            assert!(!svg.contains("stroke"), "{name}: GTK recolours fills only");
            assert!(
                svg.contains("<style type=\"text/css\" id=\"current-color-scheme\">"),
                "{name}: Plasma recolours only the style element with this id"
            );
            let alert = name.contains("alert");
            assert_eq!(svg.contains("class=\"warning\""), alert, "{name}");
            // Every drawn element is a currentColor fill, except the orange dot.
            let mut shapes = 0;
            for tag in svg
                .split('<')
                .filter(|t| t.starts_with("path") || t.starts_with("circle"))
            {
                shapes += 1;
                if tag.starts_with("circle") {
                    assert!(
                        tag.contains("class=\"warning\"") && tag.contains("fill=\"#f57900\""),
                        "{name}: {tag}"
                    );
                } else {
                    assert!(tag.contains("fill=\"currentColor\""), "{name}: {tag}");
                }
            }
            assert!(shapes >= 3, "{name}");
            let ring = svg
                .split('<')
                .find(|t| t.contains("class=\"ColorScheme-Text ring\""));
            assert!(
                ring.is_some_and(|t| t.contains("fill-rule=\"evenodd\"")),
                "{name}: ring"
            );
        }
    }

    #[test]
    fn service_down_resets_the_tray_look() {
        let _guard = cg_i18n::lock_for_tests();
        cg_i18n::set(cg_i18n::Lang::ZhCn);
        let connected = Snapshot {
            state: ConnState::Connected {
                phone_version: "2.4.0".into(),
                dns: DnsMode::Phone,
            },
            wants_connection: true,
            health: HealthStatus {
                cause: Some(IssueCause::PhoneUnstable),
                ..Default::default()
            },
            ..Snapshot::default()
        };
        let (state, mut seen) = tokio::sync::watch::channel(tray_look(None));
        show(&state, tray_look(Some(&connected)));
        assert!(seen.has_changed().unwrap());
        let _ = seen.borrow_and_update();
        // The same look again doesn't wake the tray.
        show(&state, tray_look(Some(&connected)));
        assert!(!seen.has_changed().unwrap());

        let down = ServiceDown {
            text: "服务已停止".into(),
            not_in_group: false,
            ..ServiceDown::default()
        };
        show(&state, down_look(&down));
        let look = seen.borrow_and_update().clone();
        assert!(!look.active && !look.alert && !look.wants);
        assert_eq!(look.title, "服务已停止");
        assert_eq!(
            menu_layout(&look)[2].0,
            MenuEntry::Toggle { disconnect: false }
        );
    }
}
