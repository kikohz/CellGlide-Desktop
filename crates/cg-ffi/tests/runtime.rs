//! The runtime end to end: commands in, events out, against a fake service on a real socket.
//! Plain `#[tokio::test]`s: the `Runtime` brings its own runtime, the tests only wait.

mod support;

use cellglide_native::api::{Command, Event, ReplyResult, ReplyValue};
use cellglide_native::runtime::{Config, Runtime};
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
async fn prefs_set_at_once_end_as_the_last_event_says() {
    let dir = tempfile::tempdir().unwrap();
    let (sink, events) = collect();
    let rt = Runtime::start(config_at(missing_endpoint(dir.path()), dir.path()), sink);
    for round in 0..10 {
        events.lock().unwrap().clear();
        let ids: Vec<u64> = (0..16)
            .map(|n| {
                rt.send(Command::SetPrefs {
                    prefs: Prefs {
                        include_wifi: n % 2 == 0,
                        open_on_plug: n % 3 == 0,
                        speed_confirmed: n % 5 == 0,
                    },
                })
            })
            .collect();
        for id in ids {
            assert_eq!(reply_of(&events, id).await, ReplyResult::done(), "{round}");
        }
        // Every save succeeded, and the last Prefs the app heard is what is on disk.
        let last = events.lock().unwrap().iter().rev().find_map(|e| match e {
            Event::Prefs { prefs } => Some(prefs.clone()),
            _ => None,
        });
        let saved = cg_tools::prefs::load(&dir.path().join("prefs.json"));
        assert_eq!(last, Some(saved), "round {round}");
    }
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
    // Later commands are handled as usual. (Not SetLang: `a_new_language_is_said_at_once` is the
    // only test in this binary that switches the language.)
    let id = rt.send(Command::GetPrefs);
    assert!(matches!(
        reply_of(&events, id).await,
        ReplyResult::Ok {
            value: ReplyValue::Prefs { .. }
        }
    ));
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
async fn panic_becomes_fault_and_err_reply() {
    let (sink, events) = collect();
    let ev = cellglide_native::testing::events_into(sink);
    cellglide_native::testing::guarded(7, &ev, async { panic!("boom") }).await;
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
    let ev = cellglide_native::testing::events_into(sink);
    cellglide_native::testing::guarded(8, &ev, async { ReplyResult::done() }).await;
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

use cellglide_native::testing::{SERIAL, connected_snapshot, disconnected_snapshot, middle_day};
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
        count(
            &events,
            |e| matches!(e, Event::PhonePlugged { serial } if serial == SERIAL)
        ),
        0
    );
    rt.shutdown();
}

#[tokio::test]
async fn a_service_restart_plugs_nothing_that_was_there() {
    let mut svc = FakeService::start(Replies::default()).await;
    svc.snapshots.send_replace(disconnected_snapshot());
    let (rt, events, _dir) = runtime_on(&svc);
    rt.send(Command::Subscribe);
    wait_for(&events, |e| matches!(e, Event::State { .. })).await;
    svc.stop().await;
    wait_for(&events, |e| matches!(e, Event::ServiceGone { .. })).await;
    // Back, but it hasn't listed the iPhones yet; then the same iPhone shows up again.
    svc.snapshots.send_replace(Snapshot {
        devices: Vec::new(),
        selected: None,
        ..disconnected_snapshot()
    });
    events.lock().unwrap().clear();
    svc.restart().await;
    wait_for(&events, |e| matches!(e, Event::State { .. })).await;
    events.lock().unwrap().clear();
    svc.snapshots.send_replace(disconnected_snapshot());
    wait_for(&events, |e| matches!(e, Event::State { .. })).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        count(&events, |e| matches!(e, Event::PhonePlugged { .. })),
        0
    );
    // An iPhone that was never there before is plugged in.
    svc.snapshots.send_replace(Snapshot {
        devices: vec![SERIAL.into(), "B".into()],
        ..disconnected_snapshot()
    });
    wait_for(
        &events,
        |e| matches!(e, Event::PhonePlugged { serial } if serial == "B"),
    )
    .await;
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

/// The position of the first event matching `pred`.
fn position(events: &Collected, pred: impl Fn(&Event) -> bool) -> Option<usize> {
    events.lock().unwrap().iter().position(pred)
}

#[tokio::test]
async fn a_panicking_subscription_recovers_without_a_new_subscribe() {
    let svc = FakeService::start(Replies::default()).await;
    svc.snapshots.send_replace(connected_snapshot());
    let (rt, events, _dir) = runtime_on(&svc);
    rt.send(Command::Subscribe);
    wait_for(
        &events,
        |e| matches!(e, Event::State { view } if view.connected),
    )
    .await;
    events.lock().unwrap().clear();
    cellglide_native::testing::panic_subscription(&rt, 1);
    // The next snapshot makes the subscription panic, as a bug in a view would.
    svc.snapshots.send_replace(disconnected_snapshot());
    wait_for(&events, |e| matches!(e, Event::Fault { .. })).await;
    // The stale "connected" page is replaced at once, then the service is followed again
    // without the app asking.
    let gone = wait_for(&events, |e| matches!(e, Event::ServiceGone { .. })).await;
    let Event::ServiceGone { text, .. } = gone else {
        unreachable!()
    };
    assert!(text.contains("panic"), "{text}");
    wait_for(
        &events,
        |e| matches!(e, Event::State { view } if !view.connected),
    )
    .await;
    let fault = position(&events, |e| matches!(e, Event::Fault { .. })).unwrap();
    let gone = position(&events, |e| matches!(e, Event::ServiceGone { .. })).unwrap();
    let state = position(&events, |e| matches!(e, Event::State { .. })).unwrap();
    assert!(
        fault < gone && gone < state,
        "{:#?}",
        events.lock().unwrap()
    );
    // Toggle acts on the new snapshot; the phone that was there all along wasn't plugged in.
    let id = rt.send(Command::Toggle);
    assert_eq!(reply_of(&events, id).await, ReplyResult::done());
    assert_eq!(
        count(&events, |e| matches!(e, Event::PhonePlugged { .. })),
        0
    );
    // Once the service stops, the app hears of it again.
    let mut svc = svc;
    svc.stop().await;
    wait_for(
        &events,
        |e| matches!(e, Event::ServiceGone { text, .. } if !text.contains("panic")),
    )
    .await;
    rt.shutdown();
}

#[tokio::test]
async fn persistent_panic_says_fault_and_gone_once() {
    let svc = FakeService::start(Replies::default()).await;
    svc.snapshots.send_replace(connected_snapshot());
    let (rt, events, _dir) = runtime_on(&svc);
    rt.send(Command::Subscribe);
    wait_for(
        &events,
        |e| matches!(e, Event::State { view } if view.connected),
    )
    .await;
    events.lock().unwrap().clear();
    // The next three updates panic: the first after this snapshot, then the first snapshot of
    // each of the next two retries.
    cellglide_native::testing::panic_subscription(&rt, 3);
    svc.snapshots.send_replace(disconnected_snapshot());
    wait_for(
        &events,
        |e| matches!(e, Event::State { view } if !view.connected),
    )
    .await;
    assert_eq!(
        count(&events, |e| matches!(e, Event::Fault { .. })),
        1,
        "{:#?}",
        events.lock().unwrap()
    );
    assert_eq!(
        count(&events, |e| matches!(e, Event::ServiceGone { .. })),
        1,
        "{:#?}",
        events.lock().unwrap()
    );
    events.lock().unwrap().clear();
    // Once a State was sent, a panic is news again.
    cellglide_native::testing::panic_subscription(&rt, 1);
    svc.snapshots.send_replace(connected_snapshot());
    wait_for(
        &events,
        |e| matches!(e, Event::State { view } if view.connected),
    )
    .await;
    assert_eq!(
        count(&events, |e| matches!(e, Event::Fault { .. })),
        1,
        "{:#?}",
        events.lock().unwrap()
    );
    rt.shutdown();
}

#[tokio::test]
async fn a_panicking_speed_page_follows_again() {
    let svc = FakeService::start(Replies::default()).await;
    svc.snapshots.send_replace(connected_snapshot());
    let (rt, events, _dir) = runtime_on(&svc);
    rt.send(Command::Subscribe);
    wait_for(
        &events,
        |e| matches!(e, Event::SpeedProgress { view } if view.start_enabled),
    )
    .await;
    cellglide_native::testing::panic_speed_watch_next(&rt);
    wait_for(&events, |e| matches!(e, Event::Fault { .. })).await;
    events.lock().unwrap().clear();
    // The phone goes: the page still follows, with no new Subscribe.
    svc.snapshots.send_replace(disconnected_snapshot());
    wait_for(
        &events,
        |e| matches!(e, Event::SpeedProgress { view } if !view.start_enabled),
    )
    .await;
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

#[tokio::test]
async fn a_new_language_is_said_at_once() {
    let mut svc = FakeService::start(Replies::default()).await;
    let (rt, events, _dir) = runtime_on(&svc);
    let id = rt.send(Command::SetLang { lang: "en".into() });
    reply_of(&events, id).await;
    rt.send(Command::Subscribe);
    wait_for(&events, |e| matches!(e, Event::State { .. })).await;
    let english = cg_i18n::tr_in(cg_i18n::Lang::En, "gui.connect", &[]);
    let chinese = cg_i18n::tr_in(cg_i18n::Lang::ZhCn, "gui.connect", &[]);
    events.lock().unwrap().clear();
    // Nothing changes on the service's side: only the language.
    let id = rt.send(Command::SetLang {
        lang: "zh-CN".into(),
    });
    reply_of(&events, id).await;
    wait_for(
        &events,
        |e| matches!(e, Event::State { view } if view.button_label == chinese),
    )
    .await;
    // While the service is away, ServiceGone is said again in the new language.
    svc.stop().await;
    wait_for(&events, |e| matches!(e, Event::ServiceGone { .. })).await;
    events.lock().unwrap().clear();
    let id = rt.send(Command::SetLang { lang: "en".into() });
    reply_of(&events, id).await;
    let gone = wait_for(&events, |e| matches!(e, Event::ServiceGone { .. })).await;
    let Event::ServiceGone { chrome, .. } = gone else {
        unreachable!()
    };
    assert_eq!(
        chrome.menu[2].label,
        cg_i18n::tr_in(cg_i18n::Lang::En, "tray.connect", &[])
    );
    assert_ne!(english, chinese);
    rt.shutdown();
}

// The speed test, the leak test, diagnostics upload and save (Task 10).

use cellglide_native::api::SpeedCaps;
use cellglide_native::views::{SpeedDialog, StageView};
use cg_tools::speed::engine::testkit::{FakeServer, Mode};
use cg_tools::speed::{HistoryStore, Route};
use std::path::Path;

fn speed_setup(svc: &FakeService, dir: &Path, fake: &FakeServer) -> Config {
    Config {
        speed_server: fake.server(),
        route_override: Some(Route::Unbound),
        ..config(svc, dir)
    }
}

/// A runtime with the phone connected, subscribed and past its first `State`.
async fn connected_runtime(svc: &FakeService, config: Config) -> (Runtime, Collected) {
    svc.snapshots.send_replace(connected_snapshot());
    let (sink, events) = collect();
    let rt = Runtime::start(config, sink);
    rt.send(Command::Subscribe);
    wait_for(&events, |e| matches!(e, Event::State { .. })).await;
    (rt, events)
}

fn small() -> Option<SpeedCaps> {
    Some(SpeedCaps {
        download_bytes: 200_000,
        upload_bytes: 100_000,
        seconds: 2,
    })
}

/// More than a second's worth at the fake's pace.
fn large() -> Option<SpeedCaps> {
    Some(SpeedCaps {
        download_bytes: 50_000_000,
        upload_bytes: 50_000_000,
        seconds: 10,
    })
}

fn history_in(dir: &Path) -> usize {
    HistoryStore::open(dir.join("speedtests.json"))
        .records()
        .len()
}

fn measuring(e: &Event) -> bool {
    matches!(e, Event::SpeedProgress { view }
        if view.running && view.legs[0].stage == StageView::Download)
}

#[tokio::test]
async fn speed_needs_confirmation_first() {
    let fake = FakeServer::start(Mode::Normal).await;
    let svc = FakeService::start(Replies::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let (rt, events) = connected_runtime(&svc, speed_setup(&svc, dir.path(), &fake)).await;
    let id = rt.send(Command::SpeedStart {
        include_wifi: false,
        confirmed: false,
        caps: None,
    });
    assert_eq!(reply_of(&events, id).await, ReplyResult::done());
    wait_for(&events, |e| {
        matches!(e, Event::SpeedProgress { view } if view.dialog == Some(SpeedDialog::DataWarning))
    })
    .await;
    assert_eq!(fake.accepted(), 0);
    // Cancel puts the warning away.
    events.lock().unwrap().clear();
    let id = rt.send(Command::SpeedCancel);
    assert_eq!(reply_of(&events, id).await, ReplyResult::done());
    wait_for(
        &events,
        |e| matches!(e, Event::SpeedProgress { view } if view.dialog.is_none() && !view.running),
    )
    .await;
    assert_eq!(fake.accepted(), 0);
    rt.shutdown();
}

#[tokio::test]
async fn small_speed_test_runs_and_records() {
    let fake = FakeServer::start(Mode::Normal).await;
    let svc = FakeService::start(Replies::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let (rt, events) = connected_runtime(&svc, speed_setup(&svc, dir.path(), &fake)).await;
    let id = rt.send(Command::SpeedStart {
        include_wifi: false,
        confirmed: true,
        caps: small(),
    });
    assert_eq!(reply_of(&events, id).await, ReplyResult::done());
    let last = wait_for(
        &events,
        |e| matches!(e, Event::SpeedProgress { view } if !view.running && !view.legs.is_empty()),
    )
    .await;
    let Event::SpeedProgress { view } = last else {
        unreachable!()
    };
    assert_eq!(view.legs[0].stage, StageView::Done, "{view:#?}");
    assert!(view.legs[0].phone);
    assert!(view.legs[0].download.unwrap() > 0.0 && view.legs[0].upload.unwrap() > 0.0);
    assert!(!view.legs[0].numbers.is_empty());
    assert_eq!(view.history.len(), 1);
    // The phone's carrier and radio as reported go into the history.
    assert_eq!(view.history[0].carrier.as_deref(), Some("中国移动 · 5G"));
    assert_eq!(history_in(dir.path()), 1);
    // The page said a run was going on in between.
    assert!(
        count(
            &events,
            |e| matches!(e, Event::SpeedProgress { view } if view.running)
        ) > 0
    );
    // Clearing the history empties the page and the file.
    events.lock().unwrap().clear();
    let id = rt.send(Command::SpeedHistoryClear);
    assert_eq!(reply_of(&events, id).await, ReplyResult::done());
    wait_for(
        &events,
        |e| matches!(e, Event::SpeedProgress { view } if view.history.is_empty()),
    )
    .await;
    assert_eq!(history_in(dir.path()), 0);
    rt.shutdown();
}

#[tokio::test]
async fn a_second_start_while_running_is_refused() {
    let fake = FakeServer::start(Mode::Normal).await;
    let svc = FakeService::start(Replies::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let (rt, events) = connected_runtime(&svc, speed_setup(&svc, dir.path(), &fake)).await;
    let start = || Command::SpeedStart {
        include_wifi: false,
        confirmed: true,
        caps: large(),
    };
    let first = rt.send(start());
    assert_eq!(reply_of(&events, first).await, ReplyResult::done());
    let second = rt.send(start());
    let ReplyResult::Err { message } = reply_of(&events, second).await else {
        panic!("a second run started");
    };
    assert!(in_either_language("speed.already_running").contains(&message));
    rt.send(Command::SpeedCancel);
    rt.shutdown();
}

#[tokio::test]
async fn not_connected_speed_is_text() {
    let svc = FakeService::start(Replies::default()).await;
    let dir = tempfile::tempdir().unwrap();
    // Not subscribed (no snapshot, so no phone), and no uplink.
    let (sink, events) = collect();
    let rt = Runtime::start(config(&svc, dir.path()), sink);
    let id = rt.send(Command::SpeedStart {
        include_wifi: true,
        confirmed: true,
        caps: small(),
    });
    let ReplyResult::Err { message } = reply_of(&events, id).await else {
        panic!("expected an error");
    };
    assert!(
        in_either_language("speed.error.not_connected").contains(&message),
        "{message}"
    );
    rt.shutdown();
}

#[tokio::test]
async fn cancel_stops_within_a_second_and_records_nothing() {
    // About 13 MB/s per connection: 50 MB doesn't finish within a second.
    let fake = FakeServer::start(Mode::Normal).await;
    let svc = FakeService::start(Replies::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let (rt, events) = connected_runtime(&svc, speed_setup(&svc, dir.path(), &fake)).await;
    rt.send(Command::SpeedStart {
        include_wifi: false,
        confirmed: true,
        caps: large(),
    });
    wait_for(&events, measuring).await;
    let asked = std::time::Instant::now();
    rt.send(Command::SpeedCancel);
    let ended = wait_for(&events, |e| {
        matches!(e, Event::SpeedProgress { view }
            if !view.running && view.legs.first().is_some_and(|l| l.stage == StageView::Cancelled))
    })
    .await;
    assert!(
        asked.elapsed() < Duration::from_secs(1),
        "{:?}",
        asked.elapsed()
    );
    let Event::SpeedProgress { view } = ended else {
        unreachable!()
    };
    assert!(view.history.is_empty());
    assert_eq!(history_in(dir.path()), 0);
    rt.shutdown();
}

#[test]
fn shutdown_during_speed_test_is_clean() {
    // The servers live on a runtime of the test's own; the DLL's `Runtime` is outside it.
    let servers = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (fake, svc) = servers.block_on(async {
        (
            FakeServer::start(Mode::Normal).await,
            FakeService::start(Replies::default()).await,
        )
    });
    let (rt, events) = servers.block_on(async {
        let (rt, events) = connected_runtime(&svc, speed_setup(&svc, dir.path(), &fake)).await;
        rt.send(Command::SpeedStart {
            include_wifi: false,
            confirmed: true,
            caps: large(),
        });
        wait_for(&events, measuring).await;
        (rt, events)
    });
    let t = std::time::Instant::now();
    rt.shutdown();
    assert!(
        t.elapsed() < Duration::from_millis(1500),
        "{:?}",
        t.elapsed()
    );
    let after = events.lock().unwrap().len();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        events.lock().unwrap().len(),
        after,
        "nothing after shutdown"
    );
    assert_eq!(history_in(dir.path()), 0);
    drop((fake, svc));
}

#[tokio::test]
async fn leak_reports_a_verdict() {
    static LOOKUPS: AtomicUsize = AtomicUsize::new(0);
    let target = fake_http(200, ONE_RESOLVER).await;
    let svc = FakeService::start(Replies::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        leak_target: target,
        // No real DNS: the names are only counted.
        resolve: |_| {
            LOOKUPS.fetch_add(1, SeqCst);
            Box::pin(async {})
        },
        ..config(&svc, dir.path())
    };
    let (rt, events) = connected_runtime(&svc, config).await;
    let id = rt.send(Command::LeakStart);
    assert_eq!(reply_of(&events, id).await, ReplyResult::done());
    wait_for(
        &events,
        |e| matches!(e, Event::LeakProgress { view } if view.running),
    )
    .await;
    let done = wait_for(
        &events,
        |e| matches!(e, Event::LeakProgress { view } if !view.running && view.result.is_some()),
    )
    .await;
    let Event::LeakProgress { view } = done else {
        unreachable!()
    };
    let result = view.result.unwrap();
    assert!(!result.title.is_empty());
    assert_eq!(result.groups.len(), 1);
    assert!(in_either_language("leak.again").contains(&view.start_label));
    assert_eq!(LOOKUPS.load(SeqCst), cg_tools::dns_leak::NAMES);
    rt.shutdown();
}

#[tokio::test]
async fn leak_server_error_is_the_unavailable_sentence() {
    let target = fake_http(503, "").await;
    let svc = FakeService::start(Replies::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let (sink, events) = collect();
    let rt = Runtime::start(
        Config {
            leak_target: target,
            ..config(&svc, dir.path())
        },
        sink,
    );
    rt.send(Command::LeakStart);
    let done = wait_for(
        &events,
        |e| matches!(e, Event::LeakProgress { view } if view.error.is_some()),
    )
    .await;
    let Event::LeakProgress { view } = done else {
        unreachable!()
    };
    assert!(!view.running);
    assert!(in_either_language("leak.unavailable").contains(view.error.as_ref().unwrap()));
    rt.shutdown();
}

#[tokio::test]
async fn leak_cancel_stops_the_test() {
    let svc = FakeService::start(Replies::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let (sink, events) = collect();
    let rt = Runtime::start(
        Config {
            // Lookups that never finish.
            resolve: |_| Box::pin(std::future::pending()),
            ..config(&svc, dir.path())
        },
        sink,
    );
    rt.send(Command::LeakStart);
    wait_for(
        &events,
        |e| matches!(e, Event::LeakProgress { view } if view.running),
    )
    .await;
    let asked = std::time::Instant::now();
    let id = rt.send(Command::LeakCancel);
    assert_eq!(reply_of(&events, id).await, ReplyResult::done());
    let stopped = wait_for(
        &events,
        |e| matches!(e, Event::LeakProgress { view } if !view.running),
    )
    .await;
    assert!(
        asked.elapsed() < Duration::from_secs(1),
        "{:?}",
        asked.elapsed()
    );
    let Event::LeakProgress { view } = stopped else {
        unreachable!()
    };
    assert!(view.result.is_none() && view.error.is_none());
    rt.shutdown();
}

#[test]
fn shutdown_during_leak_test_is_clean() {
    // The fake service lives on a runtime of the test's own; the DLL's `Runtime` is outside it.
    let servers = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let svc = servers.block_on(FakeService::start(Replies::default()));
    let (sink, events) = collect();
    let rt = Runtime::start(
        Config {
            // As `system_resolve`: each lookup holds one of tokio's blocking threads, here
            // for far longer than shutdown waits.
            resolve: |_| {
                Box::pin(async {
                    let _ =
                        tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_secs(5)))
                            .await;
                })
            },
            ..config(&svc, dir.path())
        },
        sink,
    );
    rt.send(Command::LeakStart);
    servers.block_on(wait_for(
        &events,
        |e| matches!(e, Event::LeakProgress { view } if view.running),
    ));
    let t = std::time::Instant::now();
    rt.shutdown();
    assert!(
        t.elapsed() < Duration::from_millis(1500),
        "{:?}",
        t.elapsed()
    );
    let after = events.lock().unwrap().len();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        events.lock().unwrap().len(),
        after,
        "nothing after shutdown"
    );
    drop(svc);
}

#[tokio::test]
async fn diagnostics_save_and_send() {
    let upload = fake_http(200, r#"{"code":"CG-ABCDE"}"#).await;
    let svc = FakeService::start(Replies::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let downloads = dir.path().join("dl");
    std::fs::create_dir(&downloads).unwrap();
    let (sink, events) = collect();
    let rt = Runtime::start(
        Config {
            download_dir: Some(downloads.clone()),
            diag_target: diag_target(&upload),
            ..config(&svc, dir.path())
        },
        sink,
    );
    let save = |rt: &Runtime| {
        rt.send(Command::DiagnosticsSave {
            json: "{}".into(),
            path: None,
            overwrite: false,
        })
    };
    let id = save(&rt);
    let ReplyResult::Ok {
        value: ReplyValue::Saved { path: first },
    } = reply_of(&events, id).await
    else {
        panic!()
    };
    assert!(Path::new(&first).starts_with(&downloads), "{first}");
    let id = save(&rt);
    let ReplyResult::Ok {
        value: ReplyValue::Saved { path: second },
    } = reply_of(&events, id).await
    else {
        panic!()
    };
    // The same second again (`-2`) or the next second: never over the first.
    assert_ne!(first, second);
    assert!(Path::new(&first).exists() && Path::new(&second).exists());
    // To a path of the user's choosing, never over an existing file.
    let chosen = dir.path().join("mine.json").to_string_lossy().into_owned();
    let id = rt.send(Command::DiagnosticsSave {
        json: "{}".into(),
        path: Some(chosen.clone()),
        overwrite: false,
    });
    assert_eq!(
        reply_of(&events, id).await,
        ReplyResult::Ok {
            value: ReplyValue::Saved {
                path: chosen.clone()
            }
        }
    );
    let id = rt.send(Command::DiagnosticsSave {
        json: "{}".into(),
        path: Some(chosen.clone()),
        overwrite: false,
    });
    assert!(matches!(
        reply_of(&events, id).await,
        ReplyResult::Err { .. }
    ));
    // The user confirmed replacing it (the save dialog asked): it is replaced.
    let id = rt.send(Command::DiagnosticsSave {
        json: "{\"again\":1}".into(),
        path: Some(chosen.clone()),
        overwrite: true,
    });
    assert_eq!(
        reply_of(&events, id).await,
        ReplyResult::Ok {
            value: ReplyValue::Saved {
                path: chosen.clone()
            }
        }
    );
    assert_eq!(std::fs::read_to_string(&chosen).unwrap(), "{\"again\":1}");
    let id = rt.send(Command::DiagnosticsSend { json: "{}".into() });
    assert_eq!(
        reply_of(&events, id).await,
        ReplyResult::Ok {
            value: ReplyValue::Sent {
                code: "CG-ABCDE".into()
            }
        }
    );
    rt.shutdown();
}

#[tokio::test]
async fn diagnostics_send_failures_are_the_window_sentences() {
    let svc = FakeService::start(Replies::default()).await;
    let dir = tempfile::tempdir().unwrap();
    for (status, key) in [(429, "diag.too_frequent"), (500, "diag.failed_gui")] {
        let upload = fake_http(status, "").await;
        let (sink, events) = collect();
        let rt = Runtime::start(
            Config {
                diag_target: diag_target(&upload),
                ..config(&svc, dir.path())
            },
            sink,
        );
        let id = rt.send(Command::DiagnosticsSend { json: "{}".into() });
        let ReplyResult::Err { message } = reply_of(&events, id).await else {
            panic!("{status} was not an error");
        };
        assert!(
            in_either_language(key).contains(&message),
            "{status}: {message}"
        );
        rt.shutdown();
    }
}

#[tokio::test]
async fn uplink_change_refreshes_the_speed_view() {
    static UPLINK_ON: AtomicBool = AtomicBool::new(false);
    let svc = FakeService::start(Replies::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        uplink: || UPLINK_ON.load(SeqCst).then(|| "Wi-Fi".to_string()),
        tick: Duration::from_millis(100),
        ..config(&svc, dir.path())
    };
    let (rt, events) = connected_runtime(&svc, config).await;
    wait_for(
        &events,
        |e| matches!(e, Event::SpeedProgress { view } if view.uplink.is_none()),
    )
    .await;
    UPLINK_ON.store(true, SeqCst);
    wait_for(
        &events,
        |e| matches!(e, Event::SpeedProgress { view } if view.uplink.as_deref() == Some("Wi-Fi")),
    )
    .await;
    rt.shutdown();
}

#[tokio::test]
async fn the_speed_page_follows_the_phone() {
    let svc = FakeService::start(Replies::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let (rt, events) = connected_runtime(&svc, config(&svc, dir.path())).await;
    // Connected: Start is on.
    wait_for(
        &events,
        |e| matches!(e, Event::SpeedProgress { view } if view.start_enabled),
    )
    .await;
    // The phone goes, and there's no Wi‑Fi either: Start goes off and says why.
    svc.snapshots.send_replace(disconnected_snapshot());
    wait_for(&events, |e| {
        matches!(e, Event::SpeedProgress { view } if !view.start_enabled && view.start_note.is_some())
    })
    .await;
    rt.shutdown();
}

#[tokio::test]
async fn each_leg_shows_its_end_as_soon_as_it_ends() {
    let fake = FakeServer::start(Mode::Normal).await;
    let svc = FakeService::start(Replies::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        uplink: || Some("Wi-Fi".to_string()),
        ..speed_setup(&svc, dir.path(), &fake)
    };
    let (rt, events) = connected_runtime(&svc, config).await;
    rt.send(Command::SpeedStart {
        include_wifi: true,
        confirmed: true,
        caps: small(),
    });
    // The phone is done while Wi‑Fi has yet to finish.
    wait_for(&events, |e| {
        matches!(e, Event::SpeedProgress { view }
            if view.running && view.legs.len() == 2
                && view.legs[0].stage == StageView::Done
                && view.legs[0].progress == 1.0
                && !view.legs[0].waiting
                && view.legs[1].stage != StageView::Done)
    })
    .await;
    let last = wait_for(
        &events,
        |e| matches!(e, Event::SpeedProgress { view } if !view.running && view.legs.len() == 2),
    )
    .await;
    let Event::SpeedProgress { view } = last else {
        unreachable!()
    };
    assert!(
        view.legs
            .iter()
            .all(|l| l.stage == StageView::Done && l.progress == 1.0)
    );
    assert!(!view.legs[1].phone);
    assert!(view.verdict.is_some(), "{view:#?}");
    rt.shutdown();
}

#[tokio::test]
async fn a_new_language_relabels_the_speed_legs() {
    let fake = FakeServer::start(Mode::Normal).await;
    let svc = FakeService::start(Replies::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        uplink: || Some("Wi-Fi".to_string()),
        ..speed_setup(&svc, dir.path(), &fake)
    };
    let (rt, events) = connected_runtime(&svc, config).await;
    let id = rt.send(Command::SetLang {
        lang: "zh-CN".into(),
    });
    reply_of(&events, id).await;
    rt.send(Command::SpeedStart {
        include_wifi: true,
        confirmed: true,
        caps: small(),
    });
    wait_for(
        &events,
        |e| matches!(e, Event::SpeedProgress { view } if !view.running && view.legs.len() == 2),
    )
    .await;
    let wifi = |lang| cg_i18n::tr_in(lang, "speed.wifi", &[("iface", "Wi-Fi")]);
    let phone = |lang| cg_i18n::tr_in(lang, "speed.phone", &[]);
    // Another test in this binary may switch the language too, and the page is only sent when
    // it reads differently: so what counts is the latest page, whatever came before.
    for (tag, lang) in [("en", cg_i18n::Lang::En), ("zh-CN", cg_i18n::Lang::ZhCn)] {
        let id = rt.send(Command::SetLang { lang: tag.into() });
        reply_of(&events, id).await;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let latest = events.lock().unwrap().iter().rev().find_map(|e| match e {
                Event::SpeedProgress { view } if view.legs.len() == 2 => {
                    Some((view.legs[0].label.clone(), view.legs[1].label.clone()))
                }
                _ => None,
            });
            if latest == Some((phone(lang), wifi(lang))) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "{tag}: {latest:?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    assert_ne!(
        wifi(cg_i18n::Lang::En),
        wifi(cg_i18n::Lang::ZhCn),
        "the two languages read differently"
    );
    rt.shutdown();
}

#[tokio::test]
async fn caps_with_nothing_to_measure_are_refused() {
    let fake = FakeServer::start(Mode::Normal).await;
    let svc = FakeService::start(Replies::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let (rt, events) = connected_runtime(&svc, speed_setup(&svc, dir.path(), &fake)).await;
    let good = small().unwrap();
    for caps in [
        SpeedCaps {
            seconds: 0,
            ..good.clone()
        },
        SpeedCaps {
            download_bytes: 0,
            ..good.clone()
        },
        SpeedCaps {
            upload_bytes: 0,
            ..good.clone()
        },
    ] {
        let id = rt.send(Command::SpeedStart {
            include_wifi: false,
            confirmed: true,
            caps: Some(caps.clone()),
        });
        let reply = reply_of(&events, id).await;
        assert!(
            matches!(reply, ReplyResult::Err { .. }),
            "{caps:?}: {reply:?}"
        );
    }
    assert_eq!(
        count(
            &events,
            |e| matches!(e, Event::SpeedProgress { view } if view.running)
        ),
        0
    );
    rt.shutdown();
}
