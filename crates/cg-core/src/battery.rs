//! The iPhone's battery as its own gauge reports it: the AppleSmartBattery registry entry,
//! read over USB through the diagnostics relay. Measured, not estimated.

use crate::lockdown::{self, LockdownInfo, Relay};
use crate::mux::MuxStream;
use crate::phone::Phone;
use crate::retry::backoff;
use crate::{CoreError, Result};
use cg_ipc::phone::{BatteryReading, UsbPhone};
use cg_protocol::phone::{BatteryReport, BatteryReportSim, port};
use plist::{Dictionary, Value};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

/// How often the battery is read while a read succeeds.
pub const READ_INTERVAL: Duration = Duration::from_secs(10);
/// The longest wait after failed reads or sends (the shared backoff tops out here).
pub const MAX_RETRY: Duration = Duration::from_secs(30);
/// A write to the phone that takes longer than this has failed.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Registry numbers, read the way Foundation's NSNumber does: integers, reals and booleans.
fn int(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Integer(i) => i.as_signed().or_else(|| {
            i.as_unsigned()
                .map(|u| i64::try_from(u).unwrap_or(i64::MAX))
        }),
        Value::Real(r) => Some(*r as i64),
        Value::Boolean(b) => Some(i64::from(*b)),
        _ => None,
    }
}

fn flag(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Boolean(b)) => *b,
        Some(Value::Real(r)) => *r != 0.0,
        other => int(other).is_some_and(|i| i != 0),
    }
}

fn positive(value: Option<i64>) -> Option<u32> {
    value.filter(|v| *v > 0).and_then(|v| u32::try_from(v).ok())
}

/// A reading from the AppleSmartBattery entry; `None` when it has none of the fields this needs.
pub fn reading_from_registry(registry: &Dictionary) -> Option<BatteryReading> {
    let empty = Dictionary::new();
    let section = |key: &str| {
        registry
            .get(key)
            .and_then(Value::as_dictionary)
            .unwrap_or(&empty)
    };
    let battery = section("BatteryData");
    let adapter = section("AdapterDetails");
    let telemetry = section("PowerTelemetryData");
    let top = |key: &str| int(registry.get(key));

    let level = top("CurrentCapacity").map(|l| l.clamp(0, 100) as u8);
    if level.is_none() && !registry.contains_key("Voltage") {
        return None;
    }
    let charging = flag(registry.get("IsCharging"));
    let external_connected = flag(registry.get("ExternalConnected"));
    let fully_charged = flag(registry.get("FullyCharged"));
    let wireless = flag(adapter.get("IsWireless"));
    let adapter_watts = external_connected
        .then(|| positive(int(adapter.get("Watts"))))
        .flatten();
    let voltage = top("Voltage");
    let battery_power = int(telemetry.get("BatteryPower")).filter(|mw| *mw > 0);
    let amperage = top("InstantAmperage").or_else(|| top("Amperage"));
    let watts = if !charging {
        None
    } else if let Some(milliwatts) = battery_power {
        Some(milliwatts as f64 / 1000.0)
    } else {
        match (voltage, amperage) {
            (Some(mv), Some(ma)) if ma > 0 => Some(mv as f64 * ma as f64 / 1_000_000.0),
            _ => None,
        }
    };
    let input_watts = external_connected
        .then(|| int(telemetry.get("SystemPowerIn")).filter(|mw| *mw > 0))
        .flatten()
        .map(|mw| mw as f64 / 1000.0);
    let design = int(battery.get("DesignCapacity")).or_else(|| top("DesignCapacity"));
    let full = int(battery.get("NominalChargeCapacity")).or_else(|| top("NominalChargeCapacity"));
    let health = match (design, full) {
        (Some(d), Some(f)) if d > 0 && f > 0 => Some((f as f64 / d as f64).min(1.0)),
        _ => None,
    };
    let cycles = top("CycleCount").and_then(|c| u32::try_from(c).ok());
    // Hundredths of a degree Celsius; 0 or absurd values mean the gauge didn't say.
    let temperature = top("Temperature")
        .filter(|t| (100..=8000).contains(t))
        .map(|t| t as f64 / 100.0);
    // Energy still missing over the power going in. Charging slows near full, so this reads
    // a little short; the phone's own estimate does the same.
    let capacity = int(battery.get("FullChargeCapacity")).or(full);
    let remaining =
        int(battery.get("RemainingCapacity")).or_else(|| int(battery.get("TrueRemainingCapacity")));
    let minutes_to_full = match (watts, capacity, remaining, voltage) {
        (Some(w), Some(c), Some(r), Some(mv)) if w >= 0.5 && c > r => {
            let missing_wh = (c - r) as f64 * mv as f64 / 1_000_000.0;
            Some(((missing_wh / w * 60.0).round() as u32).max(1))
        }
        _ => None,
    };
    Some(BatteryReading {
        level,
        charging,
        external_connected,
        fully_charged,
        watts,
        adapter_watts,
        wireless,
        health,
        cycles,
        design_capacity: positive(design),
        full_charge_capacity: positive(full),
        input_watts,
        minutes_to_full,
        temperature,
    })
}

/// What the phone app is sent on port 8091, as the Mac builds it; `None` without a battery reading.
pub fn report(usb: &UsbPhone) -> Option<BatteryReport> {
    let reading = usb.battery.as_ref()?;
    let sims = usb
        .sims
        .iter()
        .map(|sim| BatteryReportSim {
            bundle: sim.bundle.clone(),
            mcc: sim.mcc.clone(),
            mnc: sim.mnc.clone(),
        })
        .collect::<Vec<_>>();
    Some(BatteryReport {
        level: reading.level.map(i64::from),
        charging: Some(reading.charging),
        external_connected: Some(reading.external_connected),
        fully_charged: Some(reading.fully_charged),
        watts: reading.watts,
        input_watts: reading.input_watts,
        minutes_to_full: reading.minutes_to_full.map(i64::from),
        adapter_watts: reading.adapter_watts.map(i64::from),
        wireless: Some(reading.wireless),
        health: reading.health,
        cycles: reading.cycles.map(i64::from),
        design_capacity: reading.design_capacity.map(i64::from),
        full_charge_capacity: reading.full_charge_capacity.map(i64::from),
        sims: (!sims.is_empty()).then_some(sims),
        ..BatteryReport::default()
    })
}

/// One read: opens the relay when none is open, then asks the gauge. The relay goes back
/// into `open` only when the read worked; after any error it is dropped, because a timeout
/// partway through a frame leaves the stream out of step.
async fn read_once(phone: &Phone, open: &mut Option<(Relay, LockdownInfo)>) -> Result<UsbPhone> {
    let (mut relay, info) = match open.take() {
        Some(opened) => opened,
        None => {
            let pairing = lockdown::pairing(&phone.endpoint, &phone.device.serial).await?;
            lockdown::open_relay(phone, &pairing).await?
        }
    };
    let battery = relay.battery().await?;
    let usb = UsbPhone {
        model: info.model.clone(),
        os_version: info.os_version.clone(),
        sims: info.sims.clone(),
        baseband: info.baseband.clone(),
        sim_tray: info.sim_tray.clone(),
        battery,
    };
    *open = Some((relay, info));
    Ok(usb)
}

/// Reads the phone's battery over lockdown until the receiver goes away: `Some` for each
/// good read, `None` for each failure, which also waits out the shared backoff. One relay
/// connection serves all reads; after an error it is dropped and opened again. Stopping
/// (receiver closed, or the task aborted) drops the relay, closing its socket and TLS session.
pub async fn watch(phone: Phone, tx: mpsc::Sender<Option<UsbPhone>>) {
    let mut open: Option<(Relay, LockdownInfo)> = None;
    let mut failures = 0u32;
    let mut logged = false;
    loop {
        let read = tokio::select! {
            read = read_once(&phone, &mut open) => read,
            () = tx.closed() => return,
        };
        let (update, delay) = match read {
            Ok(usb) => {
                failures = 0;
                logged = false;
                (Some(usb), READ_INTERVAL)
            }
            Err(error) => {
                failures += 1;
                if !logged {
                    logged = true;
                    tracing::info!("battery details unavailable: {error}");
                }
                (None, backoff(failures))
            }
        };
        if tx.send(update).await.is_err() {
            break;
        }
        tokio::select! {
            () = sleep(delay) => {}
            () = tx.closed() => break,
        }
    }
    // No Goodbye here: the session's tasks are aborted together, so the relay is usually
    // dropped mid-wait. The socket simply closes, which the phone treats the same way.
}

pub type Connect =
    dyn Fn() -> Pin<Box<dyn Future<Output = Result<MuxStream>> + Send>> + Send + Sync + 'static;

/// Sends the battery to the phone app on 8091, one JSON line per report, over one connection.
/// Only the latest report is kept: a failure waits out the backoff, then sends whatever is
/// newest. Dropping the reporter ends its task and closes the connection.
pub struct Reporter {
    latest: watch::Sender<Option<BatteryReport>>,
    task: JoinHandle<()>,
}

impl Reporter {
    pub fn spawn(
        connect: impl Fn() -> Pin<Box<dyn Future<Output = Result<MuxStream>> + Send>>
        + Send
        + Sync
        + 'static,
    ) -> Reporter {
        let (latest, rx) = watch::channel(None);
        Reporter {
            latest,
            task: tokio::spawn(deliver(Box::new(connect), rx)),
        }
    }

    pub fn send(&self, report: BatteryReport) {
        self.latest.send_replace(Some(report));
    }
}

impl Drop for Reporter {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The 8091 connection for a phone.
pub fn reporter_for(phone: Phone) -> Reporter {
    Reporter::spawn(move || {
        let phone = phone.clone();
        Box::pin(async move { phone.open(port::BATTERY).await })
    })
}

async fn write_line(stream: &mut MuxStream, line: &[u8]) -> Result<()> {
    timeout(WRITE_TIMEOUT, async {
        stream.write_all(line).await?;
        stream.flush().await
    })
    .await
    .map_err(|_| CoreError::Timeout)??;
    Ok(())
}

async fn deliver(connect: Box<Connect>, mut latest: watch::Receiver<Option<BatteryReport>>) {
    let mut stream: Option<MuxStream> = None;
    let mut failures = 0u32;
    let mut logged = false;
    // Resolves for each report sent after the last one looked at; ends with the Reporter.
    while latest.changed().await.is_ok() {
        loop {
            let Some(report) = latest.borrow_and_update().clone() else {
                break;
            };
            let line = match report.line() {
                Ok(line) => line,
                Err(error) => {
                    // Retrying cannot make it smaller.
                    tracing::warn!("battery report dropped: {error}");
                    break;
                }
            };
            let sent = async {
                if stream.is_none() {
                    stream = Some(connect().await?);
                }
                write_line(stream.as_mut().expect("connected above"), &line).await
            }
            .await;
            match sent {
                Ok(()) => {
                    failures = 0;
                    logged = false;
                    break;
                }
                Err(error) => {
                    // Never reuse a connection after a failed write: the line may be half sent.
                    stream = None;
                    failures += 1;
                    if logged {
                        tracing::debug!("battery report not delivered: {error}");
                    } else {
                        logged = true;
                        tracing::info!("battery report not delivered: {error}");
                    }
                    sleep(backoff(failures)).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::battery_fixtures::{charging, dict, int};
    use cg_ipc::phone::SimCarrier;
    use plist::Value;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::{TcpListener, TcpStream};

    fn level_report(level: i64) -> BatteryReport {
        BatteryReport {
            level: Some(level),
            ..BatteryReport::default()
        }
    }

    async fn settle() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn refused_port_waits_then_retries_with_the_latest_reading() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let reporter = Reporter::spawn({
            let attempts = attempts.clone();
            move || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    if attempt < 2 {
                        return Err(CoreError::PhoneNotReady);
                    }
                    let stream = TcpStream::connect(addr).await?;
                    Ok(Box::new(stream) as MuxStream)
                })
            }
        });
        reporter.send(level_report(50));
        reporter.send(level_report(60));
        settle().await;
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "refused once, now waiting"
        );

        tokio::time::advance(Duration::from_secs(2)).await;
        settle().await;
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            2,
            "the second try came after 2 s"
        );

        tokio::time::advance(Duration::from_secs(4)).await;
        let (stream, _) = listener.accept().await.unwrap();
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            3,
            "the third try came after 4 s more"
        );
        let mut lines = BufReader::new(stream).lines();
        let line = lines.next_line().await.unwrap().unwrap();
        let report: BatteryReport = serde_json::from_str(&line).unwrap();
        assert_eq!(report.level, Some(60), "only the latest reading is sent");
        assert_eq!(report.service, "CellGlide-battery");

        // Nothing else follows on that connection, and it is not reopened.
        tokio::time::advance(Duration::from_secs(120)).await;
        settle().await;
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), lines.next_line())
                .await
                .is_err(),
            "no second line"
        );
        // The wire form ends in a newline (`lines` strips it, so check the encoder too).
        assert_eq!(level_report(60).line().unwrap().last(), Some(&b'\n'));
        assert_eq!(MAX_RETRY, Duration::from_secs(30));
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_reading_goes_over_the_same_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let reporter = Reporter::spawn({
            let attempts = attempts.clone();
            move || {
                attempts.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    let stream = TcpStream::connect(addr).await?;
                    Ok(Box::new(stream) as MuxStream)
                })
            }
        });
        reporter.send(level_report(10));
        let (stream, _) = listener.accept().await.unwrap();
        let mut lines = BufReader::new(stream).lines();
        let first = lines.next_line().await.unwrap().unwrap();
        assert!(first.contains(r#""level":10"#), "{first}");
        reporter.send(level_report(11));
        let second = lines.next_line().await.unwrap().unwrap();
        assert!(second.contains(r#""level":11"#), "{second}");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        // Dropping the reporter closes the connection.
        drop(reporter);
        assert_eq!(lines.next_line().await.unwrap(), None);
    }

    fn read(registry: &plist::Dictionary) -> BatteryReading {
        reading_from_registry(registry).expect("a reading")
    }

    #[test]
    fn charging_from_a_computer() {
        let reading = read(&charging());
        assert_eq!(reading.level, Some(75));
        assert!(reading.charging && reading.external_connected && !reading.fully_charged);
        assert!((reading.watts.unwrap() - 2.562).abs() < 0.001);
        assert_eq!(reading.adapter_watts, Some(15));
        assert!(!reading.wireless);
        assert!((reading.health.unwrap() - 0.949).abs() < 0.001);
        assert_eq!(reading.cycles, Some(396));
        assert_eq!(reading.design_capacity, Some(3116));
        assert_eq!(reading.full_charge_capacity, Some(2957));
        // (2994 − 2237) mAh × 4.207 V ≈ 3.18 Wh at 2.56 W ≈ 75 min.
        assert_eq!(reading.minutes_to_full, Some(75));
        assert_eq!(reading.input_watts, None);
    }

    #[test]
    fn temperature_is_in_hundredths_of_a_degree() {
        let mut registry = charging();
        registry.insert("Temperature".into(), int(3815));
        assert!((read(&registry).temperature.unwrap() - 38.15).abs() < 0.001);
        assert_eq!(read(&charging()).temperature, None);
        registry.insert("Temperature".into(), int(0));
        assert_eq!(read(&registry).temperature, None, "0 means not reported");
    }

    #[test]
    fn telemetry_gives_input_and_battery_power() {
        let mut registry = charging();
        registry.insert(
            "PowerTelemetryData".into(),
            Value::Dictionary(dict([
                ("SystemPowerIn", int(4977)),
                ("BatteryPower", int(2563)),
                ("SystemLoad", int(2414)),
            ])),
        );
        let reading = read(&registry);
        assert_eq!(reading.input_watts, Some(4.977));
        assert_eq!(reading.watts, Some(2.563));
    }

    #[test]
    fn drawing_more_than_the_supply_gives_no_charging_power() {
        let mut registry = charging();
        registry.insert("InstantAmperage".into(), int(-320));
        registry.insert("Amperage".into(), int(-300));
        registry.insert("IsCharging".into(), Value::Boolean(false));
        let reading = read(&registry);
        assert_eq!(reading.watts, None);
        assert!(reading.external_connected && !reading.charging);
    }

    #[test]
    fn unplugged_has_no_adapter() {
        let mut registry = charging();
        registry.insert("ExternalConnected".into(), Value::Boolean(false));
        registry.insert("IsCharging".into(), Value::Boolean(false));
        let reading = read(&registry);
        assert_eq!(reading.adapter_watts, None);
        assert_eq!(reading.watts, None);
    }

    #[test]
    fn full_and_wireless() {
        let mut registry = charging();
        registry.insert("FullyCharged".into(), Value::Boolean(true));
        registry.insert("IsCharging".into(), Value::Boolean(false));
        registry.insert("CurrentCapacity".into(), int(100));
        registry.insert(
            "AdapterDetails".into(),
            Value::Dictionary(dict([
                ("Watts", int(7)),
                ("IsWireless", Value::Boolean(true)),
            ])),
        );
        let reading = read(&registry);
        assert!(reading.fully_charged && reading.wireless);
        assert_eq!(reading.adapter_watts, Some(7));
        assert_eq!(reading.watts, None);
        assert_eq!(reading.minutes_to_full, None);
    }

    #[test]
    fn odd_values_are_bounded() {
        let reading = read(&dict([
            ("CurrentCapacity", int(140)),
            ("CycleCount", int(-1)),
            (
                "BatteryData",
                Value::Dictionary(dict([
                    ("DesignCapacity", int(3000)),
                    ("NominalChargeCapacity", int(3300)),
                ])),
            ),
        ]));
        assert_eq!(reading.level, Some(100));
        assert_eq!(reading.cycles, None);
        assert_eq!(reading.health, Some(1.0));
        assert_eq!(reading_from_registry(&dict([("Unrelated", int(1))])), None);
    }

    #[test]
    fn report_matches_the_mac_and_carries_sims() {
        let usb = UsbPhone {
            model: Some("iPhone18,4".into()),
            sims: vec![SimCarrier {
                bundle: Some("com.apple.ATT_aio_NR_US".into()),
                mcc: Some("310".into()),
                mnc: Some("410".into()),
            }],
            battery: Some(read(&charging())),
            ..UsbPhone::default()
        };
        let report = report(&usb).unwrap();
        assert!(report.is_valid());
        assert_eq!(report.level, Some(75));
        assert_eq!(report.charging, Some(true));
        assert_eq!(report.external_connected, Some(true));
        assert_eq!(report.fully_charged, Some(false));
        assert_eq!(report.wireless, Some(false));
        assert_eq!(report.adapter_watts, Some(15));
        assert_eq!(report.cycles, Some(396));
        assert_eq!(report.design_capacity, Some(3116));
        assert_eq!(report.full_charge_capacity, Some(2957));
        assert_eq!(report.minutes_to_full, Some(75));
        assert_eq!(report.input_watts, None);
        let sims = report.sims.unwrap();
        assert_eq!(sims.len(), 1);
        assert_eq!(sims[0].bundle.as_deref(), Some("com.apple.ATT_aio_NR_US"));
        assert_eq!(sims[0].mnc.as_deref(), Some("410"));

        let no_sims = UsbPhone {
            battery: Some(read(&charging())),
            ..UsbPhone::default()
        };
        assert_eq!(super::report(&no_sims).unwrap().sims, None);
        assert_eq!(super::report(&UsbPhone::default()), None);
    }
}
