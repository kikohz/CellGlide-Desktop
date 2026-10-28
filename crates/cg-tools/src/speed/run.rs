//! Which ways out one speed test measures, and what it leaves in the history. Ported from the
//! Mac's SpeedRun.swift: phone first, then Wi‑Fi/Ethernet one after the other so they don't
//! share bandwidth; Wi‑Fi alone when no phone is connected.

use super::engine::SpeedResult;
use super::history::{Record, Summary};
use super::transport::{Route, SpeedError};
use jiff::Timestamp;
use uuid::Uuid;

/// The routes of one test, in order: the phone (`cellglide0`) when `phone`, then `wifi`.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub phone: bool,
    /// The physical interface to compare with.
    pub wifi: Option<String>,
    /// Why the comparison was asked for and can't run: `NoInterface("")` when no Wi‑Fi or
    /// Ethernet has an up default route (only the phone or USB tethering is connected).
    /// The phone leg runs anyway; the comparison is never measured on an unbound socket.
    pub wifi_unavailable: Option<SpeedError>,
}

impl Plan {
    /// The routes to measure, in order.
    pub fn routes(&self) -> Vec<Route> {
        let phone = self.phone.then_some(Route::Phone);
        let wifi = self.wifi.clone().map(Route::Interface);
        phone.into_iter().chain(wifi).collect()
    }
}

/// Phone first, then the uplink when there is one and the user wants it. Without a phone,
/// the uplink alone whatever the switch says (it's the only thing to test); `None` with neither.
pub fn plan(phone_connected: bool, uplink: Option<String>, include_wifi: bool) -> Option<Plan> {
    if !phone_connected {
        return uplink.map(|wifi| Plan {
            phone: false,
            wifi: Some(wifi),
            wifi_unavailable: None,
        });
    }
    let wifi_unavailable =
        (include_wifi && uplink.is_none()).then(|| SpeedError::NoInterface(String::new()));
    Some(Plan {
        phone: true,
        wifi: uplink.filter(|_| include_wifi),
        wifi_unavailable,
    })
}

/// The data warning is about phone data: only a plan with the phone leg needs it.
pub fn needs_confirmation(plan: &Plan, confirmed: bool) -> bool {
    plan.phone && !confirmed
}

fn summary(result: &SpeedResult) -> Summary {
    Summary {
        download: result.download,
        upload: result.upload,
        latency: result.latency,
    }
}

/// The history entry. Pass a leg's result only when it finished (`Stage::Done`). When the
/// phone leg was tried (`had_phone`) it must have finished, or nothing is recorded; a Wi‑Fi-only
/// run is recorded when Wi‑Fi finished, without carrier or generation.
pub fn record(
    phone: Option<&SpeedResult>,
    wifi: Option<&SpeedResult>,
    had_phone: bool,
    date: Timestamp,
    carrier: Option<String>,
    generation: Option<String>,
) -> Option<Record> {
    if had_phone {
        let phone = phone?;
        return Some(Record {
            id: Uuid::new_v4(),
            date,
            phone: Some(summary(phone)),
            wifi: wifi.map(summary),
            carrier,
            generation,
        });
    }
    let wifi = wifi?;
    Some(Record {
        id: Uuid::new_v4(),
        date,
        phone: None,
        wifi: Some(summary(wifi)),
        carrier: None,
        generation: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wlan() -> Option<String> {
        Some("wlp2s0".into())
    }

    #[test]
    fn no_phone_and_no_wifi_means_no_plan() {
        assert_eq!(plan(false, None, true), None);
        assert_eq!(plan(false, None, false), None);
    }

    #[test]
    fn no_phone_with_wifi_is_wifi_only() {
        // Wi‑Fi is the only thing to test, whatever the switch says.
        for include in [true, false] {
            let p = plan(false, wlan(), include).unwrap();
            assert_eq!(p.routes(), vec![Route::Interface("wlp2s0".into())]);
            assert_eq!(p.wifi_unavailable, None);
        }
    }

    #[test]
    fn only_the_phone_path_needs_the_data_warning() {
        let wifi_only = plan(false, wlan(), true).unwrap();
        let both = plan(true, wlan(), true).unwrap();
        assert!(!needs_confirmation(&wifi_only, false));
        assert!(needs_confirmation(&both, false));
        assert!(!needs_confirmation(&both, true));
    }

    #[test]
    fn phone_without_wifi_is_phone_only() {
        let p = plan(true, None, false).unwrap();
        assert_eq!(p.routes(), vec![Route::Phone]);
        assert_eq!(p.wifi_unavailable, None, "no comparison was asked for");
    }

    #[test]
    fn missing_uplink_is_reported_and_the_phone_still_runs() {
        // Review Focus 4: comparison asked for, no physical default route.
        let p = plan(true, None, true).unwrap();
        assert_eq!(p.routes(), vec![Route::Phone]);
        assert_eq!(
            p.wifi_unavailable,
            Some(SpeedError::NoInterface(String::new()))
        );
        assert!(!p.routes().contains(&Route::Unbound));
    }

    #[test]
    fn wifi_on_adds_the_interface() {
        let p = plan(true, wlan(), true).unwrap();
        assert_eq!(
            p.routes(),
            vec![Route::Phone, Route::Interface("wlp2s0".into())]
        );
    }

    #[test]
    fn wifi_off_is_phone_only() {
        assert_eq!(
            plan(true, wlan(), false).unwrap().routes(),
            vec![Route::Phone]
        );
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

    #[test]
    fn phone_only_run_records_carrier_and_generation() {
        let phone = finished(20.0);
        let r = record(
            Some(&phone),
            None,
            true,
            now(),
            Some("中国移动".into()),
            Some("5G".into()),
        )
        .unwrap();
        assert_eq!(r.phone.unwrap().download, Some(20.0));
        assert_eq!(r.wifi, None);
        assert_eq!(
            (r.carrier.as_deref(), r.generation.as_deref()),
            (Some("中国移动"), Some("5G"))
        );
        assert_eq!(r.date, now());
    }

    #[test]
    fn phone_drop_is_not_recorded() {
        // The phone leg was tried and failed (or dropped mid-test): no history row, even with
        // a finished Wi‑Fi leg.
        let wifi = finished(60.0);
        assert_eq!(record(None, Some(&wifi), true, now(), None, None), None);
        assert_eq!(record(None, None, true, now(), None, None), None);
    }

    #[test]
    fn wifi_failure_still_records_the_phone() {
        let phone = finished(20.0);
        let r = record(Some(&phone), None, true, now(), None, None).unwrap();
        assert!(r.phone.is_some());
        assert_eq!(r.wifi, None);
    }

    #[test]
    fn both_paths_record_both() {
        let (phone, wifi) = (finished(20.0), finished(60.0));
        let r = record(Some(&phone), Some(&wifi), true, now(), None, None).unwrap();
        assert_eq!(r.wifi.unwrap().download, Some(60.0));
    }

    #[test]
    fn wifi_only_run_records_without_a_phone() {
        let wifi = finished(60.0);
        let r = record(None, Some(&wifi), false, now(), Some("x".into()), None).unwrap();
        assert_eq!(r.phone, None);
        assert_eq!(r.wifi.unwrap().download, Some(60.0));
        assert_eq!(r.carrier, None, "no phone, no carrier");
    }

    #[test]
    fn failed_wifi_only_run_is_not_recorded() {
        assert_eq!(record(None, None, false, now(), None, None), None);
    }
}
