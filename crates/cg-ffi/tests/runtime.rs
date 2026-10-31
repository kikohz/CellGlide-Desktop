//! The runtime end to end: commands in, events out, against a fake service on a real socket.
//! Plain `#[tokio::test]`s: the `Runtime` brings its own runtime, the tests only wait.

mod support;

use cellglide::api::{Command, Event, ReplyResult, ReplyValue};
use cellglide::runtime::{Config, Runtime};
use cg_ipc::Request;
use cg_tools::prefs::Prefs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::time::Duration;
use support::*;

/// A runtime that can reach no service and keeps no files.
fn offline_config() -> Config {
    let dir = unused_dir();
    config_at(missing_endpoint(&dir), &dir)
}

#[tokio::test]
async fn get_and_set_prefs_round_trip() {
    let (svc, dir) = (
        FakeService::start(Replies::default()).await,
        tempfile::tempdir().unwrap(),
    );
    let (sink, events) = collect();
    let rt = Runtime::start(config(&svc, dir.path()), sink);
    let prefs = Prefs {
        include_wifi: false,
        ..Prefs::default()
    };
    let id = rt.send(Command::SetPrefs {
        prefs: prefs.clone(),
    });
    assert_eq!(
        reply_of(&events, id).await,
        ReplyResult::Ok {
            value: ReplyValue::Done
        }
    );
    wait_for(&events, |e| {
        *e == Event::Prefs {
            prefs: prefs.clone(),
        }
    })
    .await;
    let id = rt.send(Command::GetPrefs);
    assert_eq!(
        reply_of(&events, id).await,
        ReplyResult::Ok {
            value: ReplyValue::Prefs {
                prefs: prefs.clone()
            }
        }
    );
    assert_eq!(cg_tools::prefs::load(&dir.path().join("prefs.json")), prefs);
    rt.shutdown();
}

#[tokio::test]
async fn the_prefs_event_follows_the_reply() {
    let dir = tempfile::tempdir().unwrap();
    let (sink, events) = collect();
    let rt = Runtime::start(config_at(missing_endpoint(dir.path()), dir.path()), sink);
    let id = rt.send(Command::SetPrefs {
        prefs: Prefs::default(),
    });
    wait_for(&events, |e| matches!(e, Event::Prefs { .. })).await;
    let got = events.lock().unwrap().clone();
    let reply = got
        .iter()
        .position(|e| matches!(e, Event::Reply { id: got, .. } if *got == id))
        .unwrap();
    let prefs = got
        .iter()
        .position(|e| matches!(e, Event::Prefs { .. }))
        .unwrap();
    assert!(reply < prefs, "{got:#?}");
    rt.shutdown();
}

#[tokio::test]
async fn prefs_that_cannot_be_saved_are_an_error_and_no_event() {
    let dir = tempfile::tempdir().unwrap();
    // A file where the preferences' directory should be.
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"").unwrap();
    let mut config = config_at(missing_endpoint(dir.path()), dir.path());
    config.prefs_path = blocker.join("prefs.json");
    let (sink, events) = collect();
    let rt = Runtime::start(config, sink);
    let id = rt.send(Command::SetPrefs {
        prefs: Prefs::default(),
    });
    let ReplyResult::Err { message } = reply_of(&events, id).await else {
        panic!("expected Err");
    };
    assert!(!message.is_empty());
    let id = rt.send(Command::GetPrefs);
    reply_of(&events, id).await;
    assert!(
        !events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, Event::Prefs { .. }))
    );
    rt.shutdown();
}

#[tokio::test]
async fn bad_command_replies_error() {
    let (svc, dir) = (
        FakeService::start(Replies::default()).await,
        tempfile::tempdir().unwrap(),
    );
    let (sink, events) = collect();
    let rt = Runtime::start(config(&svc, dir.path()), sink);
    let id = rt.send_json(br#"{"type":"nope"}"#);
    assert!(id > 0);
    assert!(matches!(
        reply_of(&events, id).await,
        ReplyResult::Err { .. }
    ));
    // Not JSON, and a field of the wrong type.
    for json in [&b"{"[..], br#"{"type":"set_lang","lang":5}"#] {
        let id = rt.send_json(json);
        assert!(matches!(
            reply_of(&events, id).await,
            ReplyResult::Err { .. }
        ));
    }
    rt.shutdown();
}

#[tokio::test]
async fn non_utf8_command_replies_error() {
    let (svc, dir) = (
        FakeService::start(Replies::default()).await,
        tempfile::tempdir().unwrap(),
    );
    let (sink, events) = collect();
    let rt = Runtime::start(config(&svc, dir.path()), sink);
    let id = rt.send_json(&[0xff, 0xfe]);
    assert!(matches!(
        reply_of(&events, id).await,
        ReplyResult::Err { .. }
    ));
    // Later commands are handled as usual.
    let id = rt.send(Command::SetLang { lang: "en".into() });
    assert_eq!(
        reply_of(&events, id).await,
        ReplyResult::Ok {
            value: ReplyValue::Done
        }
    );
    rt.shutdown();
}

#[tokio::test]
async fn numbers_count_up_from_one() {
    let (sink, _events) = collect();
    let rt = Runtime::start(offline_config(), sink);
    assert_eq!(rt.send(Command::GetPrefs), 1);
    assert_eq!(rt.send_json(b"nope"), 2);
    assert_eq!(rt.send_json(br#"{"type":"get_prefs"}"#), 3);
    rt.shutdown();
}

#[tokio::test]
async fn request_reaches_the_service() {
    let (svc, dir) = (
        FakeService::start(Replies::default()).await,
        tempfile::tempdir().unwrap(),
    );
    let (sink, events) = collect();
    let rt = Runtime::start(config(&svc, dir.path()), sink);
    let id = rt.send(Command::Request {
        request: Request::Connect { serial: None },
    });
    assert_eq!(
        reply_of(&events, id).await,
        ReplyResult::Ok {
            value: ReplyValue::Done
        }
    );
    assert!(svc.requests().contains(&Request::Connect { serial: None }));
    rt.shutdown();
}

#[tokio::test]
async fn a_refusal_is_the_reason_in_words() {
    let replies = Replies {
        connect: cg_ipc::Response::Error {
            reason: cg_ipc::Reason::NotAllowed,
        },
        ..Replies::default()
    };
    let (svc, dir) = (
        FakeService::start(replies).await,
        tempfile::tempdir().unwrap(),
    );
    let (sink, events) = collect();
    let rt = Runtime::start(config(&svc, dir.path()), sink);
    let id = rt.send(Command::Request {
        request: Request::Connect { serial: None },
    });
    let ReplyResult::Err { message } = reply_of(&events, id).await else {
        panic!("expected Err");
    };
    // The language may be switched by another test meanwhile.
    let texts = [cg_i18n::Lang::ZhCn, cg_i18n::Lang::En]
        .map(|lang| cg_i18n::tr_in(lang, "reason.not_allowed", &[]));
    assert!(texts.contains(&message), "{message}");
    rt.shutdown();
}

#[tokio::test]
async fn request_to_a_missing_service_is_text() {
    let mut svc = FakeService::start(Replies::default()).await;
    svc.stop().await;
    let dir = tempfile::tempdir().unwrap();
    let (sink, events) = collect();
    let rt = Runtime::start(config(&svc, dir.path()), sink);
    let started = std::time::Instant::now();
    let id = rt.send(Command::Request {
        request: Request::Status,
    });
    let ReplyResult::Err { message } = reply_of(&events, id).await else {
        panic!("expected Err")
    };
    assert!(!message.is_empty());
    assert!(started.elapsed() < Duration::from_secs(10));
    rt.shutdown();
}

#[tokio::test]
async fn a_restarted_service_is_reached_again() {
    let mut svc = FakeService::start(Replies::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let (sink, events) = collect();
    let rt = Runtime::start(config(&svc, dir.path()), sink);
    svc.stop().await;
    let id = rt.send(Command::Request {
        request: Request::Disconnect,
    });
    assert!(matches!(
        reply_of(&events, id).await,
        ReplyResult::Err { .. }
    ));
    svc.restart().await;
    let id = rt.send(Command::Request {
        request: Request::Disconnect,
    });
    assert_eq!(reply_of(&events, id).await, ReplyResult::done());
    assert_eq!(svc.requests(), [Request::Disconnect]);
    rt.shutdown();
}

#[tokio::test]
async fn commands_of_later_tasks_say_not_yet() {
    let (sink, events) = collect();
    let rt = Runtime::start(offline_config(), sink);
    let id = rt.send(Command::Subscribe);
    assert_eq!(
        reply_of(&events, id).await,
        ReplyResult::err("not yet".to_string())
    );
    rt.shutdown();
}

#[tokio::test]
async fn panic_becomes_fault_and_err_reply() {
    let (sink, events) = collect();
    let ev = cellglide::testing::events_into(sink);
    cellglide::testing::guarded(7, &ev, async { panic!("boom") }).await;
    let got = events.lock().unwrap().clone();
    assert!(
        got.iter()
            .any(|e| matches!(e, Event::Fault { message } if message.contains("boom")))
    );
    assert!(got.iter().any(|e| matches!(
        e,
        Event::Reply {
            id: 7,
            result: ReplyResult::Err { .. }
        }
    )));
}

#[tokio::test]
async fn guarded_passes_the_result_on() {
    let (sink, events) = collect();
    let ev = cellglide::testing::events_into(sink);
    cellglide::testing::guarded(8, &ev, async { ReplyResult::done() }).await;
    assert_eq!(
        *events.lock().unwrap(),
        [Event::Reply {
            id: 8,
            result: ReplyResult::done()
        }]
    );
}

#[test]
fn no_events_after_shutdown() {
    let done = Arc::new(AtomicBool::new(false));
    let late = Arc::new(AtomicBool::new(false));
    let (d, l) = (done.clone(), late.clone());
    let rt = Runtime::start(
        offline_config(),
        Box::new(move |_| {
            if d.load(SeqCst) {
                l.store(true, SeqCst)
            }
        }),
    );
    for _ in 0..10 {
        rt.send(Command::GetPrefs);
    }
    rt.shutdown();
    done.store(true, SeqCst);
    std::thread::sleep(Duration::from_millis(300));
    assert!(!late.load(SeqCst));
}

#[test]
fn events_before_shutdown_are_all_delivered() {
    let (sink, events) = collect();
    let rt = Runtime::start(offline_config(), sink);
    let ids: Vec<u64> = (0..10).map(|_| rt.send(Command::GetPrefs)).collect();
    // Wait until every command has replied, then shut down: nothing is lost on the way out.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while events.lock().unwrap().len() < ids.len() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    rt.shutdown();
    assert_eq!(events.lock().unwrap().len(), ids.len());
}

#[test]
fn shutdown_from_the_event_thread_is_safe() {
    use std::sync::Mutex;
    let slot: Arc<Mutex<Option<Runtime>>> = Arc::default();
    let after = Arc::new(AtomicBool::new(false));
    let stopped = Arc::new(AtomicBool::new(false));
    let (s, a, st) = (slot.clone(), after.clone(), stopped.clone());
    let rt = Runtime::start(
        offline_config(),
        Box::new(move |_| {
            if st.load(SeqCst) {
                a.store(true, SeqCst);
                return;
            }
            // The app shuts down from inside its own callback.
            if let Some(rt) = s.lock().unwrap().take() {
                rt.shutdown();
                st.store(true, SeqCst);
            }
        }),
    );
    *slot.lock().unwrap() = Some(rt);
    for _ in 0..10 {
        if let Some(rt) = slot.lock().unwrap().as_ref() {
            rt.send(Command::GetPrefs);
        }
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !stopped.load(SeqCst) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(stopped.load(SeqCst));
    std::thread::sleep(Duration::from_millis(300));
    assert!(!after.load(SeqCst));
}

#[tokio::test]
async fn shutdown_from_async_context_is_safe() {
    let rt = Runtime::start(offline_config(), Box::new(|_| {}));
    rt.send(Command::GetPrefs);
    rt.shutdown(); // passes if it doesn't panic
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_from_async_context_is_safe() {
    let rt = Runtime::start(offline_config(), Box::new(|_| {}));
    rt.send(Command::GetPrefs);
    drop(rt);
}
