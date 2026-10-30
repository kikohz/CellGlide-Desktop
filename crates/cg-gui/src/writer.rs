//! The one thread that writes the GUI's files (preferences, speed test history). Jobs run one at
//! a time in the order they were submitted, so two saves never overlap on the same temporary
//! file, an older snapshot never lands after a newer one, and a history Add and Clear apply in
//! the order the user made them. The UI thread only sends a job; it never touches the disk.

use cg_tools::prefs::{self, Prefs};
use cg_tools::speed::{HistoryOp, Record, history};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};
use tokio::sync::oneshot;

type HistoryReply = Result<Vec<Record>, String>;

enum Job {
    Prefs(PathBuf, Prefs),
    History(PathBuf, HistoryOp, oneshot::Sender<HistoryReply>),
    /// Answered once every job before it is done.
    #[cfg(test)]
    Flush(Sender<()>),
}

/// Started on first use; the thread ends when the writer is dropped.
#[derive(Debug, Default)]
pub struct Writer {
    jobs: OnceLock<Sender<Job>>,
}

impl Writer {
    fn send(&self, job: Job) {
        let jobs = self.jobs.get_or_init(|| {
            let (sender, receiver) = channel::<Job>();
            std::thread::spawn(move || {
                while let Ok(first) = receiver.recv() {
                    // Whatever has queued up meanwhile: of several saves of the same file only
                    // the last matters.
                    let mut batch = vec![first];
                    batch.extend(receiver.try_iter());
                    let skip: Vec<bool> = (0..batch.len())
                        .map(|index| {
                            matches!(&batch[index], Job::Prefs(path, _)
                                if batch[index + 1..]
                                    .iter()
                                    .any(|later| matches!(later, Job::Prefs(p, _) if p == path)))
                        })
                        .collect();
                    for (job, skip) in batch.into_iter().zip(skip) {
                        if !skip {
                            run(job);
                        }
                    }
                }
            });
            sender
        });
        if jobs.send(job).is_err() {
            eprintln!("cellglide-gui: the file writer stopped; not saving");
        }
    }

    pub fn save_prefs(&self, path: PathBuf, prefs: Prefs) {
        self.send(Job::Prefs(path, prefs));
    }

    /// Applies `op` to the history file after everything submitted before it.
    pub fn history(&self, path: PathBuf, op: HistoryOp) -> oneshot::Receiver<HistoryReply> {
        let (reply, receiver) = oneshot::channel();
        self.send(Job::History(path, op, reply));
        receiver
    }

    /// Blocks until everything submitted so far is on disk.
    #[cfg(test)]
    pub fn flush(&self) {
        let (done, wait) = channel();
        self.send(Job::Flush(done));
        let _ = wait.recv();
    }
}

fn run(job: Job) {
    match job {
        Job::Prefs(path, prefs) => {
            if let Err(error) = prefs::save(&path, &prefs) {
                eprintln!("cellglide-gui: can't save {}: {error}", path.display());
            }
        }
        Job::History(path, op, reply) => {
            let _ = reply.send(history::apply(path, op));
        }
        #[cfg(test)]
        Job::Flush(done) => {
            let _ = done.send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::speed;

    fn record() -> Record {
        serde_json::from_value(serde_json::json!({
            "id": "6f1c1f4e-3a52-4f0a-9d63-0e9d2f6f3f10",
            "date": "2026-10-20T12:00:00Z",
            "wifi": { "download": 60.0 },
        }))
        .unwrap()
    }

    #[test]
    fn two_quick_saves_end_with_the_newer_prefs_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cellglide/gui.json");
        let writer = Writer::default();
        for round in 0..50 {
            let older = Prefs {
                include_wifi: false,
                ..Prefs::default()
            };
            let newer = Prefs {
                speed_confirmed: true,
                open_on_plug: round % 2 == 0,
                ..Prefs::default()
            };
            writer.save_prefs(path.clone(), older);
            writer.save_prefs(path.clone(), newer.clone());
            writer.flush();
            assert_eq!(prefs::load(&path), newer);
        }
        // No temporary file is left behind.
        let left: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, vec![std::ffi::OsString::from("gui.json")]);
    }

    #[test]
    fn history_add_then_clear_ends_empty_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cellglide/speedtests.json");
        let writer = Writer::default();
        let add = writer.history(path.clone(), HistoryOp::Add(record()));
        let clear = writer.history(path.clone(), HistoryOp::Clear);
        writer.flush();
        assert_eq!(add.blocking_recv().unwrap().unwrap().len(), 1);
        assert!(clear.blocking_recv().unwrap().unwrap().is_empty());
        assert!(!path.exists());
        assert!(speed::State::new(path).records_for_test().is_empty());
    }
}
