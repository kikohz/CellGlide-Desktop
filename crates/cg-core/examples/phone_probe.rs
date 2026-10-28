//! Probes a real iPhone over the system usbmuxd: pairing record, lockdown session, TLS,
//! diagnostics relay and battery gauge. Prints no phone name and no identifiers.
//!
//!     cargo run -p cg-core --example phone_probe -- [serial]

use cg_core::lockdown::{self, Relay};
use cg_core::mux::{self, MuxEndpoint};
use cg_core::phone::Phone;
use cg_ipc::phone::BatteryReading;
use std::fmt::Display;
use std::process::ExitCode;

fn show<T: Display>(value: Option<T>) -> String {
    value.map_or_else(|| "absent".to_owned(), |v| v.to_string())
}

fn battery_line(reading: &BatteryReading) -> String {
    [
        format!("level={}", show(reading.level)),
        format!("charging={}", reading.charging),
        format!("external_connected={}", reading.external_connected),
        format!("fully_charged={}", reading.fully_charged),
        format!("watts={}", show(reading.watts)),
        format!("input_watts={}", show(reading.input_watts)),
        format!("minutes_to_full={}", show(reading.minutes_to_full)),
        format!("adapter_watts={}", show(reading.adapter_watts)),
        format!("wireless={}", reading.wireless),
        format!("health={}", show(reading.health)),
        format!("cycles={}", show(reading.cycles)),
        format!("design_capacity={}", show(reading.design_capacity)),
        format!(
            "full_charge_capacity={}",
            show(reading.full_charge_capacity)
        ),
        format!("temperature={}", show(reading.temperature)),
    ]
    .join(" ")
}

async fn read_battery(relay: &mut Relay) {
    match relay.registry().await {
        Ok(registry) => {
            match cg_core::battery::reading_from_registry(&registry) {
                Some(reading) => println!("battery: {}", battery_line(&reading)),
                None => println!("battery: absent (no level or voltage in the registry)"),
            }
            let mut keys: Vec<&str> = registry.keys().map(String::as_str).collect();
            keys.sort_unstable();
            println!("registry keys: {}", keys.join(","));
        }
        Err(error) => println!("battery: {error}"),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let endpoint = MuxEndpoint::system();
    let devices = match mux::list_devices(&endpoint).await {
        Ok(devices) => devices,
        Err(error) => {
            println!("usbmuxd: {error}");
            return ExitCode::FAILURE;
        }
    };
    let wanted = std::env::args().nth(1);
    let device = match &wanted {
        Some(serial) => devices.into_iter().find(|d| &d.serial == serial),
        None => devices.into_iter().next(),
    };
    let Some(device) = device else {
        println!("usbmuxd: no matching USB iPhone");
        return ExitCode::FAILURE;
    };
    let phone = Phone {
        endpoint: endpoint.clone(),
        device,
    };

    let pairing = match lockdown::pairing(&endpoint, &phone.device.serial).await {
        Ok(pairing) => {
            println!("pair record: ok");
            Some(pairing)
        }
        Err(error) => {
            println!("pair record: {error}");
            None
        }
    };
    let label = lockdown::describe(&phone).await;
    println!("label model: {}", show(label.model));
    let Some(pairing) = pairing else {
        return ExitCode::FAILURE;
    };

    let mut session = match lockdown::start_session(&phone, &pairing).await {
        Ok(session) => {
            println!("session: ok");
            session
        }
        Err(error) => {
            println!("session: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!("tls: {}", if session.tls() { "on" } else { "off" });
    match session.info().await {
        Ok(info) => {
            let sims = info
                .sims
                .iter()
                .map(|sim| {
                    format!(
                        "{} {}-{}",
                        sim.bundle.as_deref().unwrap_or("absent"),
                        sim.mcc.as_deref().unwrap_or("?"),
                        sim.mnc.as_deref().unwrap_or("?")
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            println!(
                "info: model={} ios={} sims={} baseband={} tray={}",
                show(info.model),
                show(info.os_version),
                if sims.is_empty() { "none".into() } else { sims },
                show(info.baseband),
                show(info.sim_tray)
            );
        }
        Err(error) => println!("info: {error}"),
    }
    let mut relay = match session.start_relay(&phone, &pairing).await {
        Ok(relay) => {
            println!("relay: ok (tls {})", if relay.tls() { "on" } else { "off" });
            relay
        }
        Err(error) => {
            println!("relay: {error}");
            return ExitCode::FAILURE;
        }
    };
    read_battery(&mut relay).await;
    relay.goodbye().await;
    ExitCode::SUCCESS
}
