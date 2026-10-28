//! What the phone card shows from the phone app's metrics: the latest sanitised frame, how
//! fresh it is, and the last minute of speeds. The phone sends its last minute once per
//! connection, then one second at a time. Mirrors the Mac app's PhoneMetricsFeed, except that
//! a second without a speed reading is a gap in the trail, never a zero.

use cg_ipc::phone::{PhoneCard, PhoneFrame, SpeedPoint, TRAIL_LENGTH, UsbPhone};
use cg_protocol::phone::PhoneMetrics;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// Dim the card after this long without a frame…
pub const STALE_AFTER: Duration = Duration::from_secs(3);
/// …and drop it after this long.
pub const GONE_AFTER: Duration = Duration::from_secs(6);

const MAX_CARRIER_CHARS: usize = 64;
const MAX_RADIO_CHARS: usize = 16;
const MAX_SPEED: f64 = 1e12;
const MAX_LATENCY: f64 = 99_999.0;
/// KB/s cap for history points (10 GB/s).
const MAX_HISTORY_SPEED: i64 = 10_000_000;
const FAILURE_KINDS: [&str; 4] = ["timeout", "refused", "dns", "other"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freshness {
    Live,
    Stale,
    Gone,
}

/// A decodable frame can still carry absurd numbers; clamp them. A reading that makes no sense
/// (not a number, a negative latency) is dropped rather than replaced by a made-up value.
pub fn sanitize(m: &PhoneMetrics) -> PhoneFrame {
    fn speed(value: Option<f64>) -> Option<f64> {
        value
            .filter(|v| v.is_finite())
            .map(|v| if v > 0.0 { v.min(MAX_SPEED) } else { 0.0 })
    }
    fn fraction(value: Option<f64>) -> Option<f64> {
        value.filter(|v| v.is_finite()).map(|v| v.clamp(0.0, 1.0))
    }
    fn latency(value: Option<f64>) -> Option<f64> {
        value
            .filter(|v| v.is_finite() && *v >= 0.0)
            .map(|v| v.min(MAX_LATENCY))
    }
    fn truncated(value: &Option<String>, max: usize) -> Option<String> {
        value.as_ref().map(|s| s.chars().take(max).collect())
    }
    PhoneFrame {
        carrier: truncated(&m.carrier, MAX_CARRIER_CHARS),
        radio: truncated(&m.radio, MAX_RADIO_CHARS),
        sharing: m.sharing,
        download: speed(m.download),
        upload: speed(m.upload),
        session_bytes: m.session_bytes.map(|b| b.max(0)),
        quality_score: m.quality_score.map(|s| s.clamp(0, 100)),
        latency: latency(m.latency),
        loss: fraction(m.loss),
        battery: fraction(m.battery),
        charging: m.charging,
        low_power: m.low_power,
        thermal: m.thermal.map(|t| t.clamp(0, 3)),
        exit_ok: m.exit_ok,
        exit_latency: latency(m.exit_latency),
        connects_ok: m.connects_ok.map(|n| n.max(0)),
        connects_failed: m.connects_failed.as_ref().map(|failed| {
            failed
                .iter()
                .filter(|(kind, _)| FAILURE_KINDS.contains(&kind.as_str()))
                .map(|(kind, n)| (kind.clone(), (*n).max(0)))
                .collect::<BTreeMap<_, _>>()
        }),
    }
}

/// The phone's own last minute (KB/s pairs, oldest first) as points in bytes per second.
/// A point that isn't exactly two numbers is a gap (never a zero) so the rest keep their
/// place in time.
fn history_points(history: &[Vec<i64>]) -> Vec<Option<SpeedPoint>> {
    let skip = history.len().saturating_sub(TRAIL_LENGTH);
    history[skip..]
        .iter()
        .map(|point| {
            let bytes = |kb: i64| kb.clamp(0, MAX_HISTORY_SPEED) as f64 * 1000.0;
            match point.as_slice() {
                [down, up] => Some(SpeedPoint {
                    download: bytes(*down),
                    upload: bytes(*up),
                }),
                _ => None,
            }
        })
        .collect()
}

#[derive(Debug, Default)]
pub struct PhoneFeed {
    latest: Option<PhoneFrame>,
    received: Option<Instant>,
    trail: Vec<Option<SpeedPoint>>,
}

impl PhoneFeed {
    pub fn receive(&mut self, m: &PhoneMetrics, now: Instant) {
        let frame = sanitize(m);
        self.received = Some(now);
        if frame.sharing != Some(true) {
            self.trail.clear();
            self.latest = Some(frame);
            return;
        }
        if let Some(history) = &m.history {
            let points = history_points(history);
            let skip = points.len().saturating_sub(TRAIL_LENGTH - 1);
            self.trail = points[skip..].to_vec();
        }
        self.trail.push(match (frame.download, frame.upload) {
            (Some(download), Some(upload)) => Some(SpeedPoint { download, upload }),
            _ => None,
        });
        if self.trail.len() > TRAIL_LENGTH {
            self.trail.drain(..self.trail.len() - TRAIL_LENGTH);
        }
        self.latest = Some(frame);
    }

    pub fn freshness(&self, now: Instant) -> Freshness {
        let (Some(_), Some(received)) = (&self.latest, self.received) else {
            return Freshness::Gone;
        };
        let age = now.saturating_duration_since(received);
        if age >= GONE_AFTER {
            Freshness::Gone
        } else if age >= STALE_AFTER {
            Freshness::Stale
        } else {
            Freshness::Live
        }
    }

    /// The card to publish. Once the phone app has gone quiet only what USB reveals is left
    /// (the Mac shows just the battery when the app isn't in front).
    pub fn card(&self, now: Instant, usb: Option<&UsbPhone>) -> Option<PhoneCard> {
        match self.freshness(now) {
            Freshness::Gone => usb.map(|usb| PhoneCard {
                usb: Some(usb.clone()),
                ..Default::default()
            }),
            freshness => Some(PhoneCard {
                frame: self.latest.clone(),
                stale: freshness == Freshness::Stale,
                trail: self.trail.clone(),
                usb: usb.cloned(),
            }),
        }
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    fn sharing(download: f64, upload: f64) -> PhoneMetrics {
        PhoneMetrics {
            sharing: Some(true),
            download: Some(download),
            upload: Some(upload),
            ..Default::default()
        }
    }

    fn point(download: f64, upload: f64) -> Option<SpeedPoint> {
        Some(SpeedPoint { download, upload })
    }

    fn trail(feed: &PhoneFeed, now: Instant) -> Vec<Option<SpeedPoint>> {
        feed.card(now, None).map(|c| c.trail).unwrap_or_default()
    }

    #[test]
    fn freshness_bands() {
        let start = Instant::now();
        let mut feed = PhoneFeed::default();
        assert_eq!(feed.freshness(start), Freshness::Gone);
        feed.receive(&PhoneMetrics::default(), start);
        let at = |s: f64| start + Duration::from_secs_f64(s);
        assert_eq!(feed.freshness(at(0.0)), Freshness::Live);
        assert_eq!(feed.freshness(at(2.9)), Freshness::Live);
        assert_eq!(feed.freshness(at(3.0)), Freshness::Stale);
        assert_eq!(feed.freshness(at(5.9)), Freshness::Stale);
        assert_eq!(feed.freshness(at(6.0)), Freshness::Gone);
    }

    #[test]
    fn trail_keeps_the_last_sixty_points() {
        let start = Instant::now();
        let mut feed = PhoneFeed::default();
        for second in 0..75u64 {
            feed.receive(
                &sharing(second as f64, 1.0),
                start + Duration::from_secs(second),
            );
        }
        let t = trail(&feed, start + Duration::from_secs(74));
        assert_eq!(t.len(), TRAIL_LENGTH);
        assert_eq!(t.first().unwrap().unwrap().download, 15.0);
        assert_eq!(t.last().unwrap().unwrap().download, 74.0);
    }

    #[test]
    fn extreme_frame_is_sanitized_before_it_is_stored() {
        let m: PhoneMetrics = serde_json::from_str(&format!(
            r#"{{"service":"CellGlide-metrics","version":1,"carrier":"{}","radio":"{}","sharing":true,"download":1e15,"upload":-5,"sessionBytes":-1,"qualityScore":150,"latency":1e9,"loss":2,"battery":-0.5,"thermal":9,"exitLatency":-1,"connectsOK":-3,"connectsFailed":{{"timeout":-2,"weird":5}}}}"#,
            "x".repeat(100),
            "y".repeat(40)
        ))
        .unwrap();
        let f = sanitize(&m);
        assert_eq!(f.carrier.unwrap().chars().count(), 64);
        assert_eq!(f.radio.unwrap().chars().count(), 16);
        assert_eq!(f.download, Some(1e12));
        assert_eq!(f.upload, Some(0.0));
        assert_eq!(f.session_bytes, Some(0));
        assert_eq!(f.quality_score, Some(100));
        assert_eq!(f.latency, Some(99_999.0));
        assert_eq!(f.loss, Some(1.0));
        assert_eq!(f.battery, Some(0.0));
        assert_eq!(f.thermal, Some(3));
        assert_eq!(f.exit_latency, None);
        assert_eq!(f.connects_ok, Some(0));
        assert_eq!(
            f.connects_failed,
            Some(BTreeMap::from([("timeout".to_string(), 0)]))
        );
    }

    #[test]
    fn non_finite_readings_are_dropped_not_zeroed() {
        let m = PhoneMetrics {
            latency: Some(f64::INFINITY),
            loss: Some(f64::NAN),
            battery: Some(f64::NEG_INFINITY),
            exit_latency: Some(f64::NAN),
            ..Default::default()
        };
        let f = sanitize(&m);
        assert_eq!(
            (f.latency, f.loss, f.battery, f.exit_latency),
            (None, None, None, None)
        );
    }

    #[test]
    fn history_fills_the_trail_on_connect() {
        let start = Instant::now();
        let mut feed = PhoneFeed::default();
        let mut m = sharing(5_000.0, 500.0);
        m.history = Some((1..=70).map(|i| vec![i, 1]).collect());
        feed.receive(&m, start);
        let t = trail(&feed, start);
        assert_eq!(t.len(), TRAIL_LENGTH);
        // The last 59 history points in KB/s become bytes per second, then the current second.
        assert_eq!(t[0], point(12_000.0, 1_000.0));
        assert_eq!(t[58], point(70_000.0, 1_000.0));
        assert_eq!(t[59], point(5_000.0, 500.0));
        feed.receive(&sharing(6_000.0, 600.0), start + Duration::from_secs(1));
        let t = trail(&feed, start + Duration::from_secs(1));
        assert_eq!(t.last().copied().flatten(), point(6_000.0, 600.0));
        assert_eq!(t.len(), TRAIL_LENGTH);
    }

    #[test]
    fn malformed_history_points_are_gaps() {
        let start = Instant::now();
        let mut feed = PhoneFeed::default();
        let mut m = sharing(0.0, 0.0);
        m.history = Some(vec![
            vec![-5, 3],
            vec![1],
            vec![i64::MAX, i64::MAX],
            vec![7, 8, 9],
        ]);
        feed.receive(&m, start);
        let cap = 10_000_000.0 * 1000.0;
        // Malformed points are gaps (ruling P16), so the rest keep their place in time.
        assert_eq!(
            trail(&feed, start),
            vec![
                point(0.0, 3_000.0),
                None,
                point(cap, cap),
                None,
                point(0.0, 0.0)
            ]
        );
        let idle = PhoneMetrics {
            sharing: Some(false),
            history: Some(vec![vec![1, 1]]),
            ..Default::default()
        };
        let later = start + Duration::from_secs(1);
        feed.receive(&idle, later);
        assert!(trail(&feed, later).is_empty());
    }

    #[test]
    fn trail_clears_when_not_sharing() {
        let start = Instant::now();
        let mut feed = PhoneFeed::default();
        feed.receive(&sharing(5.0, 1.0), start);
        let idle = PhoneMetrics {
            sharing: Some(false),
            ..Default::default()
        };
        let t1 = start + Duration::from_secs(1);
        feed.receive(&idle, t1);
        assert!(trail(&feed, t1).is_empty());
        let t2 = start + Duration::from_secs(2);
        feed.receive(&PhoneMetrics::default(), t2);
        assert!(trail(&feed, t2).is_empty());
    }

    #[test]
    fn missing_speed_is_a_gap() {
        let start = Instant::now();
        let mut feed = PhoneFeed::default();
        let m = PhoneMetrics {
            sharing: Some(true),
            ..Default::default()
        };
        feed.receive(&m, start);
        assert_eq!(trail(&feed, start), vec![None]);
    }

    #[test]
    fn reset() {
        let start = Instant::now();
        let mut feed = PhoneFeed::default();
        feed.receive(&sharing(5.0, 1.0), start);
        feed.reset();
        assert!(feed.card(start, None).is_none());
        assert_eq!(feed.freshness(start), Freshness::Gone);
    }

    #[test]
    fn health_fields_are_clamped() {
        let start = Instant::now();
        let mut feed = PhoneFeed::default();
        let m = PhoneMetrics {
            exit_latency: Some(-5.0),
            connects_ok: Some(-1),
            connects_failed: Some(
                [("timeout", -2), ("refused", 4), ("x-unknown", 9)]
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v))
                    .collect(),
            ),
            ..Default::default()
        };
        feed.receive(&m, start);
        let frame = feed.card(start, None).unwrap().frame.unwrap();
        assert_eq!(frame.exit_latency, None);
        assert_eq!(frame.connects_ok, Some(0));
        assert_eq!(
            frame.connects_failed,
            Some(BTreeMap::from([
                ("timeout".to_string(), 0),
                ("refused".to_string(), 4)
            ]))
        );
    }

    #[test]
    fn card_follows_freshness() {
        let start = Instant::now();
        let mut feed = PhoneFeed::default();
        feed.receive(&sharing(5.0, 1.0), start);
        let usb = UsbPhone {
            model: Some("iPhone18,4".into()),
            ..Default::default()
        };
        let at = |s: u64| start + Duration::from_secs(s);
        assert!(!feed.card(at(1), Some(&usb)).unwrap().stale);
        let stale = feed.card(at(4), Some(&usb)).unwrap();
        assert!(stale.stale && stale.frame.is_some());
        // The app has gone quiet: only what USB reveals is left.
        let gone = feed.card(at(7), Some(&usb)).unwrap();
        assert!(gone.frame.is_none() && gone.trail.is_empty() && gone.usb.as_ref() == Some(&usb));
        assert!(feed.card(at(7), None).is_none());
    }
}
