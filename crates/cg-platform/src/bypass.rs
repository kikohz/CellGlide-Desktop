//! Keeps the local network out of the tunnel, like the Mac's private bypass: private
//! ranges go to the original gateway, multicast and link-local stay on the original
//! interface. Every route carries realm 198 so only our own are ever deleted.

use serde_json::Value;
use std::net::Ipv4Addr;

pub const REALM: u32 = 198;
/// Worse than any route a user or NetworkManager would add for the same prefix, so ours
/// never override theirs; they still beat the tunnel's /1 routes by prefix length.
pub const METRIC: u32 = 32766;
pub const VIA_GATEWAY: [&str; 4] = [
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "100.64.0.0/10",
];
pub const ON_LINK: [&str; 2] = ["224.0.0.0/4", "169.254.0.0/16"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gateway {
    /// None for point-to-point uplinks whose default route has no gateway.
    pub via: Option<Ipv4Addr>,
    pub dev: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OurRoute {
    pub dst: String,
    pub dev: String,
    pub via: Option<Ipv4Addr>,
}

fn routes(json: &str) -> Vec<Value> {
    serde_json::from_str::<Vec<Value>>(json).unwrap_or_default()
}

fn text<'a>(route: &'a Value, key: &str) -> Option<&'a str> {
    route.get(key).and_then(Value::as_str)
}

/// A route's `dev` and `gateway`: a multipath route keeps them on its nexthops, so the
/// first nexthop stands in when the top level has none.
fn dev_and_gateway(route: &Value) -> (Option<&str>, Option<&str>) {
    let first = route
        .get("nexthops")
        .and_then(Value::as_array)
        .and_then(|n| n.first());
    let pick = |key| text(route, key).or_else(|| first.and_then(|n| text(n, key)));
    (pick("dev"), pick("gateway"))
}

/// The lowest-metric IPv4 default route that isn't on `ours`.
pub fn parse_default(json: &str, ours: &str) -> Option<Gateway> {
    routes(json)
        .iter()
        .filter(|r| {
            text(r, "dst") == Some("default") && dev_and_gateway(r).0.is_some_and(|d| d != ours)
        })
        .min_by_key(|r| r.get("metric").and_then(Value::as_u64).unwrap_or(0))
        .map(|r| {
            let (dev, gateway) = dev_and_gateway(r);
            Gateway {
                via: gateway.and_then(|g| g.parse().ok()),
                dev: dev.unwrap_or_default().to_string(),
            }
        })
}

pub fn route_args(prefix: &str, gateway: &Gateway, action: &str) -> Vec<String> {
    let mut args = vec!["route".to_string(), action.to_string(), prefix.to_string()];
    if ON_LINK.contains(&prefix) {
        args.extend([
            "dev".into(),
            gateway.dev.clone(),
            "scope".into(),
            "link".into(),
        ]);
    } else {
        if let Some(via) = gateway.via {
            args.extend(["via".into(), via.to_string()]);
        }
        args.extend(["dev".into(), gateway.dev.clone()]);
    }
    args.extend([
        "proto".into(),
        "static".into(),
        "realm".into(),
        REALM.to_string(),
        "metric".into(),
        METRIC.to_string(),
    ]);
    args
}

fn realm_of(route: &Value) -> Option<u32> {
    // iproute2 prints a route's realm as {"flow": {"to": "198"}}.
    route
        .get("flow")
        .and_then(|f| f.get("to"))
        .and_then(Value::as_str)
        .and_then(|r| r.parse().ok())
}

/// Bypass routes we added, from `ip -j -4 route show table main`.
pub fn parse_ours(json: &str) -> Vec<OurRoute> {
    routes(json)
        .iter()
        .filter(|r| realm_of(r) == Some(REALM))
        .filter_map(|r| {
            let dst = text(r, "dst")?;
            (VIA_GATEWAY.contains(&dst) || ON_LINK.contains(&dst)).then(|| OurRoute {
                dst: dst.to_string(),
                dev: text(r, "dev").unwrap_or_default().to_string(),
                via: text(r, "gateway").and_then(|g| g.parse().ok()),
            })
        })
        .collect()
}

/// What to log when the default route is (or isn't) found; None means stay quiet.
pub fn gateway_notice(missing_before: bool, found: bool) -> Option<&'static str> {
    match (missing_before, found) {
        (false, false) => Some("missing"),
        (true, true) => Some("back"),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
mod runtime {
    use super::*;
    use crate::ip;
    use cg_core::tunnel::TunnelNotice;
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;

    fn ip_json(args: &[&str]) -> String {
        Command::new("ip")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_else(|| "[]".into())
    }

    fn current_default(ours: &str) -> Option<Gateway> {
        parse_default(&ip_json(&["-j", "-4", "route", "show", "default"]), ours)
    }

    fn list_ours() -> Vec<OurRoute> {
        parse_ours(&ip_json(&["-j", "-4", "route", "show", "table", "main"]))
    }

    /// Adds every bypass route via `gateway`; false when any of them couldn't be added.
    fn add_all(gateway: &Gateway) -> bool {
        let mut all = true;
        for prefix in VIA_GATEWAY.iter().chain(ON_LINK.iter()) {
            match ip::run(&route_args(prefix, gateway, "add")) {
                Ok(()) => {}
                // Someone already routes this range; it stays out of the tunnel either way.
                Err(error) if error.0.contains("File exists") => {
                    tracing::debug!("bypass {prefix}: an existing route covers it")
                }
                Err(error) => {
                    tracing::warn!("bypass {prefix}: {error}");
                    all = false;
                }
            }
        }
        all
    }

    /// Deletes our routes that match `gateway`, after checking each one is really ours.
    fn remove_all(gateway: &Gateway) {
        let present = list_ours();
        for prefix in VIA_GATEWAY.iter().chain(ON_LINK.iter()) {
            let via = if ON_LINK.contains(prefix) {
                None
            } else {
                gateway.via
            };
            let mine = present
                .iter()
                .any(|r| r.dst == *prefix && r.dev == gateway.dev && r.via == via);
            if mine && let Err(error) = ip::run(&route_args(prefix, gateway, "del")) {
                tracing::warn!("removing bypass {prefix}: {error}");
            }
        }
    }

    /// Removes every bypass route left behind by an earlier run.
    pub fn cleanup_leftovers() {
        for route in list_ours() {
            let gateway = Gateway {
                via: route.via,
                dev: route.dev.clone(),
            };
            if let Err(error) = ip::run(&route_args(&route.dst, &gateway, "del")) {
                tracing::warn!("removing leftover bypass {}: {error}", route.dst);
            }
        }
    }

    #[derive(Default)]
    struct State {
        current: Option<Gateway>,
        /// Set while there is no default route outside our interface, so the warning is
        /// logged once.
        missing: bool,
        /// For the connection timeline, taken by `Bypass::take_notices`.
        notices: Vec<TunnelNotice>,
    }

    fn follow(ours: &str, state: &Mutex<State>) {
        let mut state = state.lock().unwrap();
        let next = current_default(ours);
        match gateway_notice(state.missing, next.is_some()) {
            Some("missing") => {
                tracing::warn!("no default route outside {ours}; keeping the bypass as it is");
                state.notices.push(TunnelNotice::BypassUnavailable);
            }
            Some("back") => {
                if let Some(next) = &next {
                    tracing::info!("default route back on {}", next.dev);
                }
            }
            _ => {}
        }
        state.missing = next.is_none();
        if let Some(next) = next {
            let moved = state.current.as_ref() != Some(&next);
            if moved {
                tracing::info!("default route moved to {next:?}; updating the bypass");
                if let Some(old) = state.current.as_ref() {
                    remove_all(old);
                }
            }
            // The kernel drops our routes silently when an address is flushed, so
            // re-adding is always safe: routes that exist report File exists.
            if !add_all(&next) && moved {
                state.notices.push(TunnelNotice::BypassUpdateFailed);
            }
            state.current = Some(next);
        }
    }

    pub struct Bypass {
        current: Arc<Mutex<State>>,
        monitor: Option<Child>,
        thread: Option<JoinHandle<()>>,
        stopping: Arc<AtomicBool>,
    }

    impl Drop for Bypass {
        /// Dropped without `stop` (a panic): don't leave the monitor child running.
        /// Routes are left to `stop` and `cleanup_leftovers`.
        fn drop(&mut self) {
            self.stopping.store(true, Ordering::SeqCst);
            if let Some(mut child) = self.monitor.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    impl Bypass {
        pub fn start(ours: &str) -> Bypass {
            cleanup_leftovers();
            let gateway = current_default(ours);
            let mut notices = Vec::new();
            match &gateway {
                Some(gateway) => {
                    tracing::info!("bypass uses {gateway:?}");
                    add_all(gateway);
                }
                None => {
                    tracing::warn!("bypass unavailable: no default route outside {ours}");
                    notices.push(TunnelNotice::BypassUnavailable);
                }
            }
            let current = Arc::new(Mutex::new(State {
                missing: gateway.is_none(),
                current: gateway,
                notices,
            }));
            let mut monitor = Command::new("ip")
                // Links and addresses too: the kernel drops a downed uplink's default
                // route and our routes on it without any route event.
                .args(["-o", "-4", "monitor", "route", "address", "link"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| tracing::warn!("can't follow route changes: {e}"))
                .ok();
            let stopping = Arc::new(AtomicBool::new(false));
            let thread = monitor
                .as_mut()
                .and_then(|child| child.stdout.take())
                .map(|stdout| {
                    let (ours, current, stopping) =
                        (ours.to_string(), current.clone(), stopping.clone());
                    std::thread::spawn(move || {
                        // Any event may mean the routes changed; follow is idempotent.
                        for _ in BufReader::new(stdout).lines().map_while(Result::ok) {
                            follow(&ours, &current);
                        }
                        if !stopping.load(Ordering::SeqCst) {
                            tracing::warn!(
                                "route monitor ended; the bypass no longer follows changes"
                            );
                        }
                    })
                });
            // Whatever changed between the first look and the monitor starting.
            follow(ours, &current);
            Bypass {
                current,
                monitor,
                thread,
                stopping,
            }
        }

        /// Notices since the last call, oldest first.
        pub fn take_notices(&self) -> Vec<TunnelNotice> {
            std::mem::take(&mut self.current.lock().unwrap().notices)
        }

        pub fn stop(mut self) {
            self.stopping.store(true, Ordering::SeqCst);
            if let Some(mut child) = self.monitor.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            if let Some(gateway) = self.current.lock().unwrap().current.take() {
                remove_all(&gateway);
            }
        }
    }
}

#[cfg(target_os = "linux")]
pub use runtime::{Bypass, cleanup_leftovers};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_notice_logs_only_on_changes() {
        assert_eq!(gateway_notice(false, false), Some("missing"));
        assert_eq!(gateway_notice(true, false), None);
        assert_eq!(gateway_notice(true, true), Some("back"));
        assert_eq!(gateway_notice(false, true), None);
    }

    const DEFAULTS: &str = r#"[
        {"dst":"default","gateway":"192.168.0.1","dev":"wlp2s0","protocol":"dhcp","metric":600,"flags":[]},
        {"dst":"default","gateway":"10.1.0.1","dev":"enp1s0","protocol":"dhcp","metric":100,"flags":[]},
        {"dst":"default","dev":"cellglide0","scope":"link","flags":[]}
    ]"#;

    #[test]
    fn picks_the_lowest_metric_default_outside_our_interface() {
        assert_eq!(
            parse_default(DEFAULTS, "cellglide0"),
            Some(Gateway {
                via: Some("10.1.0.1".parse().unwrap()),
                dev: "enp1s0".into()
            })
        );
        assert_eq!(parse_default("[]", "cellglide0"), None);
        assert_eq!(
            parse_default(
                r#"[{"dst":"default","dev":"cellglide0","flags":[]}]"#,
                "cellglide0"
            ),
            None
        );
        assert_eq!(
            parse_default(
                r#"[{"dst":"default","dev":"ppp0","scope":"link","flags":[]}]"#,
                "cellglide0"
            ),
            Some(Gateway {
                via: None,
                dev: "ppp0".into()
            })
        );
        assert_eq!(parse_default("not json", "cellglide0"), None);
    }

    #[test]
    fn multipath_default_uses_the_first_nexthop() {
        let json = r#"[{"dst":"default","protocol":"static","metric":100,"flags":[],
            "nexthops":[{"gateway":"10.1.0.1","dev":"enp1s0","weight":1,"flags":[]},
                        {"gateway":"192.168.0.1","dev":"wlp2s0","weight":1,"flags":[]}]}]"#;
        assert_eq!(
            parse_default(json, "cellglide0"),
            Some(Gateway {
                via: Some("10.1.0.1".parse().unwrap()),
                dev: "enp1s0".into()
            })
        );
        // Our own interface as the first nexthop still excludes the route.
        assert_eq!(parse_default(json, "enp1s0"), None);
    }

    #[test]
    fn parses_the_real_default_route() {
        let json = include_str!("../tests/data/default.json");
        let gateway = parse_default(json, "cellglide0").expect("default route");
        assert_eq!(gateway.dev, "eth0");
        assert_eq!(gateway.via, Some("192.168.5.2".parse().unwrap()));
    }

    #[test]
    fn route_arguments() {
        let gw = Gateway {
            via: Some("192.168.0.1".parse().unwrap()),
            dev: "wlp2s0".into(),
        };
        assert_eq!(
            route_args("10.0.0.0/8", &gw, "add").join(" "),
            "route add 10.0.0.0/8 via 192.168.0.1 dev wlp2s0 proto static realm 198 metric 32766"
        );
        assert_eq!(
            route_args("224.0.0.0/4", &gw, "del").join(" "),
            "route del 224.0.0.0/4 dev wlp2s0 scope link proto static realm 198 metric 32766"
        );
        let ppp = Gateway {
            via: None,
            dev: "ppp0".into(),
        };
        assert_eq!(
            route_args("10.0.0.0/8", &ppp, "add").join(" "),
            "route add 10.0.0.0/8 dev ppp0 proto static realm 198 metric 32766"
        );
    }

    #[test]
    fn finds_only_our_routes() {
        let json = include_str!("../tests/data/main-with-ours.json");
        let ours = parse_ours(json);
        assert!(
            ours.iter()
                .any(|r| r.dst == "10.0.0.0/8" && r.via.is_some())
        );
        assert!(
            ours.iter()
                .any(|r| r.dst == "224.0.0.0/4" && r.via.is_none())
        );
        assert_eq!(ours.len(), 2, "{ours:?}");
        assert!(
            ours.iter()
                .all(|r| VIA_GATEWAY.contains(&r.dst.as_str()) || ON_LINK.contains(&r.dst.as_str()))
        );
        assert!(!ours.iter().any(|r| r.dst == "default"));
    }
}
