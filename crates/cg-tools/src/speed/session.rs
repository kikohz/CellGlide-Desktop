//! The speed test session: which legs run and in what order, cancelling, and what goes into the
//! history. The measuring is `super::measure`; the terminal and the window both drive this.

use super::{Plan, Progress, Record, Route, SpeedResult, Stage, record};
use cg_i18n::tr;
use jiff::Timestamp;
use std::future::Future;
use std::time::Duration;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// How often the progress line is refreshed.
pub const REFRESH: Duration = Duration::from_millis(500);

/// One measured way out, as it ended.
#[derive(Clone, Debug, PartialEq)]
pub struct Leg {
    pub route: Route,
    pub label: String,
    pub stage: Stage,
    pub result: SpeedResult,
}

impl Leg {
    pub fn done(&self) -> Option<&SpeedResult> {
        (self.stage == Stage::Done).then_some(&self.result)
    }
}

/// How a run ended: the legs that were measured, in order, and whether the user cancelled.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub legs: Vec<Leg>,
    pub cancelled: bool,
}

/// The routes of `plan` with the names the user sees. Never contains `Route::Unbound`.
pub fn labelled_routes(plan: &Plan) -> Vec<(Route, String)> {
    plan.routes()
        .into_iter()
        .map(|route| {
            let label = route_label(&route);
            (route, label)
        })
        .collect()
}

/// The name the user sees for `route`, in the current language.
pub fn route_label(route: &Route) -> String {
    match route {
        Route::Interface(iface) => tr("speed.wifi", &[("iface", iface)]),
        _ => tr("speed.phone", &[]),
    }
}

/// Measures the routes one after the other. After a cancel no further leg starts and the
/// leg that was running is dropped, so a cancelled run has nothing to record.
/// `on_progress` is called with the leg index, label and progress every `REFRESH`.
pub async fn run_legs<M, F>(
    routes: Vec<(Route, String)>,
    cancel: &CancellationToken,
    mut measure: M,
    mut on_progress: impl FnMut(usize, &str, &Progress),
) -> Run
where
    M: FnMut(Route, watch::Sender<Progress>, CancellationToken) -> F,
    F: Future<Output = SpeedResult>,
{
    let mut legs = Vec::new();
    for (index, (route, label)) in routes.into_iter().enumerate() {
        if cancel.is_cancelled() {
            break;
        }
        let (tx, rx) = watch::channel(Progress::default());
        let measuring = measure(route.clone(), tx.clone(), cancel.clone());
        tokio::pin!(measuring);
        let mut tick = tokio::time::interval(REFRESH);
        let result = loop {
            tokio::select! {
                result = &mut measuring => break result,
                _ = tick.tick() => on_progress(index, &label, &rx.borrow()),
            }
        };
        if cancel.is_cancelled() {
            break;
        }
        let stage = tx.borrow().stage.clone();
        legs.push(Leg {
            route,
            label,
            stage,
            result,
        });
    }
    Run {
        legs,
        cancelled: cancel.is_cancelled(),
    }
}

/// The history entry for a finished run; none after a cancel, and none when the phone leg was
/// planned but didn't finish.
pub fn history_record(
    plan: &Plan,
    run: &Run,
    date: Timestamp,
    carrier: Option<String>,
    generation: Option<String>,
) -> Option<Record> {
    if run.cancelled {
        return None;
    }
    let finished = |phone: bool| {
        run.legs
            .iter()
            .find(|leg| (leg.route == Route::Phone) == phone)
            .and_then(Leg::done)
    };
    record(
        finished(true),
        finished(false),
        plan.phone,
        date,
        carrier,
        generation,
    )
}

#[cfg(test)]
mod tests {
    use super::super::{SpeedError, plan, text::error_text};
    use super::*;
    use std::sync::{Arc, Mutex, MutexGuard};

    fn language(lang: cg_i18n::Lang) -> MutexGuard<'static, ()> {
        let guard = cg_i18n::lock_for_tests();
        cg_i18n::set(lang);
        guard
    }

    fn finished(download: f64) -> SpeedResult {
        SpeedResult {
            latency: Some(40.0),
            download: Some(download),
            upload: Some(download / 4.0),
            ..SpeedResult::default()
        }
    }

    fn now() -> Timestamp {
        Timestamp::from_second(1_791_000_000).unwrap()
    }

    /// A measurer that reports Done with canned numbers and logs the order of starts and ends.
    fn fake(
        log: Arc<Mutex<Vec<String>>>,
        cancel_on: Option<&'static str>,
    ) -> impl FnMut(
        Route,
        watch::Sender<Progress>,
        CancellationToken,
    ) -> std::pin::Pin<Box<dyn Future<Output = SpeedResult>>> {
        move |route, tx, cancel| {
            let log = log.clone();
            Box::pin(async move {
                let name = match &route {
                    Route::Phone => "phone".to_string(),
                    Route::Interface(i) => i.clone(),
                    Route::Unbound => panic!("an unbound route was measured"),
                };
                log.lock().unwrap().push(format!("start {name}"));
                tokio::task::yield_now().await;
                let result = finished(if name == "phone" { 20.0 } else { 60.0 });
                if cancel_on == Some(if name == "phone" { "phone" } else { "wifi" }) {
                    cancel.cancel();
                    tx.send_modify(|p| p.stage = Stage::Cancelled);
                    return SpeedResult::default();
                }
                tx.send_modify(|p| {
                    p.stage = Stage::Done;
                    p.result = result.clone();
                });
                log.lock().unwrap().push(format!("end {name}"));
                result
            })
        }
    }

    #[tokio::test]
    async fn phone_then_wifi_one_after_the_other() {
        let plan = plan(true, Some("wlp2s0".into()), true).unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let run = run_legs(
            labelled_routes(&plan),
            &CancellationToken::new(),
            fake(log.clone(), None),
            |_, _, _| {},
        )
        .await;
        assert_eq!(
            *log.lock().unwrap(),
            ["start phone", "end phone", "start wlp2s0", "end wlp2s0"]
        );
        assert!(!run.cancelled);
        assert_eq!(run.legs.len(), 2);
        let rec = history_record(&plan, &run, now(), Some("X".into()), Some("5G".into())).unwrap();
        assert_eq!(rec.phone.unwrap().download, Some(20.0));
        assert_eq!(rec.wifi.unwrap().download, Some(60.0));
        assert_eq!(rec.carrier.as_deref(), Some("X"));
        assert_eq!(rec.generation.as_deref(), Some("5G"));
    }

    #[tokio::test]
    async fn cancel_skips_the_next_leg_and_records_nothing() {
        let plan = plan(true, Some("wlp2s0".into()), true).unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let run = run_legs(
            labelled_routes(&plan),
            &CancellationToken::new(),
            fake(log.clone(), Some("phone")),
            |_, _, _| {},
        )
        .await;
        assert!(run.cancelled);
        assert!(run.legs.is_empty());
        assert_eq!(*log.lock().unwrap(), ["start phone"], "Wi-Fi never started");
        assert_eq!(history_record(&plan, &run, now(), None, None), None);
    }

    #[tokio::test]
    async fn cancel_during_the_second_leg_drops_the_finished_first_from_history() {
        let plan = plan(true, Some("wlp2s0".into()), true).unwrap();
        let run = run_legs(
            labelled_routes(&plan),
            &CancellationToken::new(),
            fake(Arc::default(), Some("wifi")),
            |_, _, _| {},
        )
        .await;
        assert!(run.cancelled);
        assert_eq!(history_record(&plan, &run, now(), None, None), None);
    }

    #[tokio::test]
    async fn already_cancelled_runs_nothing() {
        let plan = plan(true, None, false).unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let log = Arc::new(Mutex::new(Vec::new()));
        let run = run_legs(
            labelled_routes(&plan),
            &cancel,
            fake(log.clone(), None),
            |_, _, _| {},
        )
        .await;
        assert!(run.cancelled && run.legs.is_empty());
        assert!(log.lock().unwrap().is_empty());
    }

    #[test]
    fn missing_uplink_never_adds_an_unbound_leg() {
        let _lang = language(cg_i18n::Lang::En);
        let plan = plan(true, None, true).unwrap();
        let routes = labelled_routes(&plan);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].0, Route::Phone);
        assert_eq!(
            plan.wifi_unavailable,
            Some(SpeedError::NoInterface(String::new()))
        );
        // The reason the user is shown for that leg.
        assert_eq!(
            error_text(plan.wifi_unavailable.as_ref().unwrap()),
            tr("speed.error.no_interface", &[])
        );
    }

    #[test]
    fn failed_phone_leg_is_not_recorded_even_when_wifi_finished() {
        let plan = plan(true, Some("wlp2s0".into()), true).unwrap();
        let run = Run {
            legs: vec![
                Leg {
                    route: Route::Phone,
                    label: "iPhone".into(),
                    stage: Stage::Failed(SpeedError::NotConnected),
                    result: SpeedResult::default(),
                },
                Leg {
                    route: Route::Interface("wlp2s0".into()),
                    label: "w".into(),
                    stage: Stage::Done,
                    result: finished(60.0),
                },
            ],
            cancelled: false,
        };
        assert_eq!(history_record(&plan, &run, now(), None, None), None);
    }
}
