//! A small-data speed measurement for investigating the engine on a real link, without the
//! 1.3 GB of a full test:
//!
//! `cargo run -p cg-tools --example speed_probe -- [--route phone|uplink|<alias>]
//!  [--down-mb N] [--up-mb N] [--seconds S] [--streams K]`
//!
//! Defaults: `--route phone --down-mb 10 --up-mb 5 --seconds 5 --streams 6`. `uplink` is this
//! computer's own physical uplink (`cg_tools::uplink::physical_uplink`). Every tick prints
//! `stage elapsed moved_bytes rate`; the end prints the result and the payload bytes moved.

use cg_tools::speed::{Params, Progress, Route, Server, Stage, measure};
use std::time::Duration;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

const MB: u64 = 1_000_000;

struct Args {
    route: Route,
    down_mb: u64,
    up_mb: u64,
    seconds: u64,
    streams: usize,
}

fn usage() -> ! {
    eprintln!(
        "usage: speed_probe [--route phone|uplink|<alias>] [--down-mb N] [--up-mb N] \
         [--seconds S] [--streams K]"
    );
    std::process::exit(2)
}

fn parse() -> Args {
    let mut args = Args {
        route: Route::Phone,
        down_mb: 10,
        up_mb: 5,
        seconds: 5,
        streams: 6,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next().unwrap_or_else(|| usage());
        let number = || value.parse::<u64>().unwrap_or_else(|_| usage());
        match flag.as_str() {
            "--route" => {
                args.route = match value.as_str() {
                    "phone" => Route::Phone,
                    "uplink" => {
                        Route::Interface(cg_tools::uplink::physical_uplink().unwrap_or_else(|| {
                            eprintln!("no physical uplink");
                            std::process::exit(1)
                        }))
                    }
                    alias => Route::Interface(alias.to_string()),
                }
            }
            "--down-mb" => args.down_mb = number(),
            "--up-mb" => args.up_mb = number(),
            "--seconds" => args.seconds = number(),
            "--streams" => args.streams = number() as usize,
            _ => usage(),
        }
    }
    args
}

fn stage_name(stage: &Stage) -> String {
    match stage {
        Stage::Failed(error) => format!("failed({error:?})"),
        other => format!("{other:?}").to_lowercase(),
    }
}

#[tokio::main]
async fn main() {
    let args = parse();
    let params = Params {
        streams: args.streams,
        duration: Duration::from_secs(args.seconds),
        download_cap: args.down_mb * MB,
        upload_cap: args.up_mb * MB,
        ..Params::default()
    };
    println!("route {:?}, {params:?}", args.route);
    let (tx, mut rx) = watch::channel(Progress::default());
    let task = tokio::spawn(measure(
        args.route,
        Server::cloudflare(),
        params,
        tx,
        CancellationToken::new(),
    ));
    // Each direction counts its own bytes; payload_bytes is the running total of both.
    let mut before = 0u64;
    let mut last_stage = Stage::Idle;
    while rx.changed().await.is_ok() {
        let p = rx.borrow_and_update().clone();
        if p.stage != last_stage {
            if p.stage == Stage::Upload {
                before = p.payload_bytes;
            }
            last_stage = p.stage.clone();
        }
        let rate = match p.stage {
            Stage::Download => p.result.download,
            Stage::Upload => p.result.upload,
            _ => None,
        };
        if let Some(rate) = rate {
            println!(
                "{} {:.1} {} {:.2}",
                stage_name(&p.stage),
                p.elapsed,
                p.payload_bytes - before,
                rate
            );
        }
    }
    let result = task.await.expect("measure task");
    println!(
        "result: stage {}, latency {:?} ms, download {:?} Mbps, upload {:?} Mbps, notes {:?}",
        stage_name(&rx.borrow().stage),
        result.latency,
        result.download,
        result.upload,
        result.notes
    );
    println!("payload bytes: {}", rx.borrow().payload_bytes);
}
