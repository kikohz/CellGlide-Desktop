//! The runtime end to end: commands in, events out, against a fake service on a real socket.
//! Plain `#[tokio::test]`s: the `Runtime` brings its own runtime, the tests only wait.

mod support;

use cellglide::api::{Command, Event, ReplyResult, ReplyValue};
use cellglide::runtime::{Config, Runtime};
use cg_ipc::Request;
use cg_tools::prefs::Prefs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
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
    let id = rt.send(Command::LeakStart);
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
    let delivered = Arc::new(AtomicUsize::new(0));
    let (d, l, n) = (done.clone(), late.clone(), delivered.clone());
    // A slow sink, so events are still queued when shutdown starts.
    let rt = Runtime::start(
        offline_config(),
        Box::new(move |_| {
            if d.load(SeqCst) {
                l.store(true, SeqCst)
            }
            n.fetch_add(1, SeqCst);
            std::thread::sleep(Duration::from_millis(20));
        }),
    );
    for _ in 0..10 {
        rt.send(Command::GetPrefs);
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while delivered.load(SeqCst) == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    let before = delivered.load(SeqCst);
    assert!(
        (1..10).contains(&before),
        "{before} delivered before shutdown"
    );
    rt.shutdown();
    done.store(true, SeqCst);
    let at_return = delivered.load(SeqCst);
    std::thread::sleep(Duration::from_millis(300));
    assert!(!late.load(SeqCst));
    assert_eq!(delivered.load(SeqCst), at_return);
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

// The subscription, Toggle, and the replies that are views (Task 9).

use cellglide::testing::{SERIAL, connected_snapshot, disconnected_snapshot, middle_day};
use cg_ipc::{Check, CheckId, Contact, DiagRange, Response, Snapshot};

/// A runtime on `svc` (files in a fresh directory), and what it says.
fn runtime_on(svc: &FakeService) -> (Runtime, Collected, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let (sink, events) = collect();
    let rt = Runtime::start(config(svc, dir.path()), sink);
    (rt, events, dir)
}

fn count(events: &Collected, pred: impl Fn(&Event) -> bool) -> usize {
    events.lock().unwrap().iter().filter(|e| pred(e)).count()
}

#[tokio::test]
async fn subscribe_emits_state_then_service_gone() {
    let mut svc = FakeService::start(Replies::default()).await;
    svc.snapshots.send_replace(connected_snapshot());
    let (rt, events, _dir) = runtime_on(&svc);
    let id = rt.send(Command::Subscribe);
    assert_eq!(reply_of(&events, id).await, ReplyResult::done());
    wait_for(
        &events,
        |e| matches!(e, Event::State { view } if view.connected),
    )
    .await;
    svc.stop().await;
    let gone = wait_for(&events, |e| matches!(e, Event::ServiceGone { .. })).await;
    assert!(matches!(gone, Event::ServiceGone { text, .. } if !text.is_empty()));
    rt.shutdown();
}

#[tokio::test]
async fn service_restart_resumes_state() {
    // config.retry is 100 ms in real time: the subscription runs on the DLL's own runtime.
    let mut svc = FakeService::start(Replies::default()).await;
    svc.snapshots.send_replace(disconnected_snapshot());
    let (rt, events, _dir) = runtime_on(&svc);
    rt.send(Command::Subscribe);
    wait_for(&events, |e| matches!(e, Event::State { .. })).await;
    svc.stop().await;
    wait_for(&events, |e| matches!(e, Event::ServiceGone { .. })).await;
    events.lock().unwrap().clear();
    svc.restart().await;
    svc.snapshots.send_replace(Snapshot {
        devices: vec![SERIAL.into(), "B".into()],
        ..disconnected_snapshot()
    });
    wait_for(&events, |e| matches!(e, Event::State { .. })).await;
    // Toggle acts on the new round's snapshot.
    let id = rt.send(Command::Toggle);
    assert_eq!(reply_of(&events, id).await, ReplyResult::done());
    assert_eq!(
        count(&events, |e| matches!(e, Event::PhonePlugged { .. })),
        0
    );
    rt.shutdown();
}

#[tokio::test]
async fn service_gone_is_not_repeated_on_every_retry() {
    let mut svc = FakeService::start(Replies::default()).await;
    let (rt, events, _dir) = runtime_on(&svc);
    rt.send(Command::Subscribe);
    wait_for(&events, |e| matches!(e, Event::State { .. })).await;
    svc.stop().await;
    wait_for(&events, |e| matches!(e, Event::ServiceGone { .. })).await;
    // Several 100 ms retries go by. The reason is said again only when it changes (the
    // subscription ending, then the service not answering).
    tokio::time::sleep(Duration::from_millis(600)).await;
    let texts: Vec<String> = events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            Event::ServiceGone { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!((1..=2).contains(&texts.len()), "{texts:#?}");
    assert!(texts.windows(2).all(|w| w[0] != w[1]), "{texts:#?}");
    // While the service is away Toggle has no snapshot to act on.
    let id = rt.send(Command::Toggle);
    assert!(matches!(
        reply_of(&events, id).await,
        ReplyResult::Err { .. }
    ));
    rt.shutdown();
}

#[tokio::test]
async fn a_service_that_never_ran_is_gone_until_it_comes() {
    let mut svc = FakeService::start(Replies::default()).await;
    svc.stop().await;
    let (rt, events, _dir) = runtime_on(&svc);
    rt.send(Command::Subscribe);
    wait_for(&events, |e| matches!(e, Event::ServiceGone { .. })).await;
    svc.restart().await;
    wait_for(&events, |e| matches!(e, Event::State { .. })).await;
    rt.shutdown();
}

#[tokio::test]
async fn plugged_phone_emits_once() {
    let svc = FakeService::start(Replies::default()).await;
    let nothing_plugged = || Snapshot {
        devices: Vec::new(),
        selected: None,
        ..disconnected_snapshot()
    };
    svc.snapshots.send_replace(nothing_plugged());
    let (rt, events, _dir) = runtime_on(&svc);
    rt.send(Command::Subscribe);
    wait_for(&events, |e| matches!(e, Event::State { .. })).await;
    svc.snapshots.send_replace(Snapshot {
        devices: vec!["A".into()],
        ..nothing_plugged()
    });
    wait_for(
        &events,
        |e| matches!(e, Event::PhonePlugged { serial } if serial == "A"),
    )
    .await;
    // Several ticks go by.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        count(&events, |e| matches!(e, Event::PhonePlugged { .. })),
        1
    );
    rt.shutdown();
}

#[tokio::test]
async fn subscribing_twice_keeps_one_subscription() {
    let svc = FakeService::start(Replies::default()).await;
    let (rt, events, _dir) = runtime_on(&svc);
    for _ in 0..3 {
        let id = rt.send(Command::Subscribe);
        assert_eq!(reply_of(&events, id).await, ReplyResult::done());
    }
    wait_for(&events, |e| matches!(e, Event::State { .. })).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let subscriptions = svc
        .requests()
        .iter()
        .filter(|r| **r == Request::Subscribe)
        .count();
    assert_eq!(subscriptions, 1);
    // Nothing changes, so the ticks send nothing more.
    assert_eq!(count(&events, |e| matches!(e, Event::State { .. })), 1);
    rt.shutdown();
}

#[tokio::test]
async fn toggle_follows_the_wish() {
    let svc = FakeService::start(Replies::default()).await;
    svc.snapshots.send_replace(Snapshot {
        wants_connection: true,
        ..connected_snapshot()
    });
    let (rt, events, _dir) = runtime_on(&svc);
    rt.send(Command::Subscribe);
    wait_for(&events, |e| matches!(e, Event::State { .. })).await;
    let id = rt.send(Command::Toggle);
    assert_eq!(
        reply_of(&events, id).await,
        ReplyResult::Ok {
            value: ReplyValue::Done
        }
    );
    assert!(svc.requests().contains(&Request::Disconnect));
    // Nobody wants a connection: Toggle connects.
    svc.snapshots.send_replace(disconnected_snapshot());
    wait_for(
        &events,
        |e| matches!(e, Event::State { view } if !view.wants_connection),
    )
    .await;
    let id = rt.send(Command::Toggle);
    assert_eq!(reply_of(&events, id).await, ReplyResult::done());
    assert!(svc.requests().contains(&Request::Connect { serial: None }));
    rt.shutdown();
}

#[tokio::test]
async fn toggle_before_any_snapshot_is_text() {
    let (sink, events) = collect();
    let rt = Runtime::start(offline_config(), sink);
    let id = rt.send(Command::Toggle);
    let ReplyResult::Err { message } = reply_of(&events, id).await else {
        panic!("expected Err");
    };
    // The language may be switched by another test meanwhile.
    let texts = [cg_i18n::Lang::ZhCn, cg_i18n::Lang::En]
        .map(|lang| cg_i18n::tr_in(lang, "gui.waiting_service", &[]));
    assert!(texts.contains(&message), "{message}");
    rt.shutdown();
}

#[tokio::test]
async fn doctor_and_history_replies_are_views() {
    let checks = vec![
        Check::new(CheckId::Service, true),
        Check::new(CheckId::Phone, false),
    ];
    let svc = FakeService::start(Replies {
        doctor: Response::Doctor {
            checks: checks.clone(),
        },
        history: Response::History { day: middle_day() },
        ..Replies::default()
    })
    .await;
    let (rt, events, _dir) = runtime_on(&svc);
    let id = rt.send(Command::Request {
        request: Request::Doctor,
    });
    let ReplyResult::Ok {
        value: ReplyValue::Checks { rows },
    } = reply_of(&events, id).await
    else {
        panic!("expected Checks")
    };
    assert_eq!(rows.len(), 2);
    assert!(rows[1].fix.is_some() && rows[0].fix.is_none());
    let id = rt.send(Command::Request {
        request: Request::History {
            day: Some(middle_day().day),
        },
    });
    let ReplyResult::Ok {
        value: ReplyValue::History { view },
    } = reply_of(&events, id).await
    else {
        panic!("expected History")
    };
    assert_eq!(view.day, middle_day().day);
    assert_eq!(view.rows.len(), middle_day().events.len());
    rt.shutdown();
}

#[tokio::test]
async fn diagnostics_prepare_trims_the_contact() {
    let svc = FakeService::start(Replies::default()).await;
    let (rt, events, _dir) = runtime_on(&svc);
    let id = rt.send(Command::DiagnosticsPrepare {
        range: DiagRange::Day,
        email: "  a@b.co ".into(),
        note: "".into(),
    });
    let ReplyResult::Ok {
        value: ReplyValue::Diagnostics { file_name, json },
    } = reply_of(&events, id).await
    else {
        panic!("expected Diagnostics")
    };
    assert!(file_name.starts_with("cellglide-diagnostics-"));
    assert_eq!(json, "{}");
    let contact = Contact {
        email: Some("a@b.co".into()),
        note: None,
    };
    assert!(svc.requests().contains(&Request::Diagnostics {
        range: DiagRange::Day,
        contact: Some(contact)
    }));
    // Nothing typed: no contact at all.
    let id = rt.send(Command::DiagnosticsPrepare {
        range: DiagRange::Week,
        email: " ".into(),
        note: "".into(),
    });
    reply_of(&events, id).await;
    assert!(svc.requests().contains(&Request::Diagnostics {
        range: DiagRange::Week,
        contact: None
    }));
    rt.shutdown();
}

#[test]
fn every_key_the_dll_names_is_listed() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    assert_eq!(
        cg_i18n::testing::unlisted_keys(&[root.join("src")]),
        Vec::<(std::path::PathBuf, String)>::new()
    );
}
