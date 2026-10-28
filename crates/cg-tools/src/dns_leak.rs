//! The DNS leak test: resolve random names under leak.cellglide.com through this computer's
//! own DNS, then ask the CellGlide server which resolvers came asking for them. Only those
//! random names and the fetch are sent; the server keeps what it saw for five minutes.
//! Ported from the Mac's DNSLeakTest.swift (server: Scripts/dnsleak-server.py).

use crate::ToolError;
use crate::https::{self, Target};
use cg_i18n::{Lang, tr_in};
use cg_ipc::DnsMode;
use futures::future::{BoxFuture, join_all};
use serde::Deserialize;
use std::time::Duration;

pub const ZONE: &str = "leak.cellglide.com";
pub const NAMES: usize = 20;
pub const RESOLVE_TIMEOUT: Duration = Duration::from_secs(6);
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

impl Target {
    /// The leak test's result server.
    pub fn leakcheck() -> Target {
        Target {
            host: "leakcheck.cellglide.com".into(),
            port: 443,
            tls: true,
        }
    }
}

/// One network the server saw: the address the results were fetched from, or a resolver.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Network {
    pub ip: String,
    pub asn: Option<u32>,
    pub org: Option<String>,
    pub country: Option<String>,
    pub region: Option<String>,
    /// Resolvers only: how many test names it asked for.
    pub queries: Option<u32>,
    /// Resolvers only: the client subnet it passed on (EDNS Client Subnet).
    pub subnet: Option<String>,
}

impl Network {
    /// "Virginia, US"; the country alone when no region is known.
    pub fn location(&self) -> Option<String> {
        let country = self.country.as_ref()?;
        match self.region.as_deref() {
            Some(region) if !region.is_empty() => Some(format!("{region}, {country}")),
            _ => Some(country.clone()),
        }
    }

    /// The owner's name, or the address when the owner is unknown.
    fn name(&self) -> String {
        self.org.clone().unwrap_or_else(|| self.ip.clone())
    }
}

/// What the server saw (Scripts/dnsleak-server.py).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Report {
    pub you: Network,
    pub resolvers: Vec<Network>,
}

impl Report {
    /// Resolvers that told the sites' DNS servers which network this computer is on.
    pub fn passes_subnet(&self) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for resolver in self.resolvers.iter().filter(|r| r.subnet.is_some()) {
            let name = resolver.name();
            if !seen.contains(&name) {
                seen.push(name);
            }
        }
        seen
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Every query went to an encrypted DNS provider, the phone's choice.
    Encrypted { providers: Vec<String> },
    /// The exit network's own DNS answered: encrypted DNS was unreachable, or turned off.
    Carrier { network: String },
    /// Resolvers belonging to neither: the queries reached them some other way.
    Leak { networks: Vec<String> },
    /// The computer isn't online through the phone, so this shows its own network's DNS.
    NotSharing,
    /// No resolver asked for the test names: blocked, or the server was unreachable.
    NothingSeen,
    /// Linux only: the system wouldn't let CellGlide set DNS, so lookups use the original network.
    Unmanaged { networks: Vec<String> },
}

/// Networks of the encrypted DNS services CellGlide offers, and their usual names.
const PROVIDERS: &[(u32, &str)] = &[
    (13335, "Cloudflare"),
    (15169, "Google"),
    (36692, "OpenDNS"),
    (19281, "Quad9"),
    (42, "Quad9"),
    (37963, "AliDNS"),
    (45102, "AliDNS"),
    (132203, "DNSPod"),
    (45090, "DNSPod"),
];

fn provider(asn: u32) -> Option<&'static str> {
    PROVIDERS.iter().find(|(a, _)| *a == asn).map(|(_, n)| *n)
}

fn push_unique(list: &mut Vec<String>, name: String) {
    if !list.contains(&name) {
        list.push(name);
    }
}

/// `sharing` is the DNS mode of the running connection; `None` means this computer is not
/// online through the phone.
pub fn judge(report: &Report, sharing: Option<DnsMode>) -> Verdict {
    if report.resolvers.is_empty() {
        return Verdict::NothingSeen;
    }
    let Some(mode) = sharing else {
        return Verdict::NotSharing;
    };
    if mode == DnsMode::Unmanaged {
        let mut networks = Vec::new();
        for resolver in &report.resolvers {
            push_unique(&mut networks, resolver.name());
        }
        return Verdict::Unmanaged { networks };
    }
    let (mut encrypted, mut carrier, mut unknown) = (Vec::new(), None, Vec::new());
    for resolver in &report.resolvers {
        match resolver.asn {
            Some(asn) if provider(asn).is_some() => {
                push_unique(&mut encrypted, provider(asn).unwrap().to_string());
            }
            Some(asn) if report.you.asn == Some(asn) => carrier = Some(resolver.name()),
            _ => push_unique(&mut unknown, resolver.name()),
        }
    }
    if !unknown.is_empty() {
        Verdict::Leak { networks: unknown }
    } else if let Some(network) = carrier {
        Verdict::Carrier { network }
    } else {
        Verdict::Encrypted {
            providers: encrypted,
        }
    }
}

impl Verdict {
    /// One value of every variant (data-carrying ones with a sample); adding a variant fails to
    /// compile until the match below names it.
    #[cfg(test)]
    pub(crate) fn all() -> Vec<Verdict> {
        let all = vec![
            Verdict::Encrypted {
                providers: vec!["Cloudflare".into(), "Google".into()],
            },
            Verdict::Carrier {
                network: "CARRIER".into(),
            },
            Verdict::Leak {
                networks: vec!["NET-A".into(), "NET-B".into()],
            },
            Verdict::NotSharing,
            Verdict::NothingSeen,
            Verdict::Unmanaged {
                networks: vec!["NET-A".into()],
            },
        ];
        for verdict in &all {
            match verdict {
                Verdict::Encrypted { .. }
                | Verdict::Carrier { .. }
                | Verdict::Leak { .. }
                | Verdict::NotSharing
                | Verdict::NothingSeen
                | Verdict::Unmanaged { .. } => {}
            }
        }
        all
    }

    /// The middle part of this verdict's text keys: `leak.<name>.title` and `.detail`.
    fn key_name(&self) -> &'static str {
        match self {
            Verdict::Encrypted { .. } => "encrypted",
            Verdict::Carrier { .. } => "carrier",
            Verdict::Leak { .. } => "leak",
            Verdict::NotSharing => "not_sharing",
            Verdict::NothingSeen => "nothing",
            Verdict::Unmanaged { .. } => "unmanaged",
        }
    }
}

/// A verdict's title and explanation in `lang`.
pub fn describe(verdict: &Verdict, lang: Lang) -> (String, String) {
    let join = |list: &[String]| list.join(&tr_in(lang, "list_separator", &[]));
    let name = verdict.key_name();
    let args: Vec<(&str, String)> = match verdict {
        Verdict::Encrypted { providers } => vec![("providers", join(providers))],
        Verdict::Carrier { network } => vec![("network", network.clone())],
        Verdict::Leak { networks } | Verdict::Unmanaged { networks } => {
            vec![("networks", join(networks))]
        }
        Verdict::NotSharing | Verdict::NothingSeen => Vec::new(),
    };
    let args: Vec<(&str, &str)> = args.iter().map(|(k, v)| (*k, v.as_str())).collect();
    (
        tr_in(lang, &format!("leak.{name}.title"), &[]),
        tr_in(lang, &format!("leak.{name}.detail"), &args),
    )
}

/// The resolvers that asked, merged by owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolverGroup {
    pub name: String,
    /// Where the first resolver of the group is.
    pub location: Option<String>,
    pub addresses: Vec<String>,
    /// Test names asked for, summed over the group; `None` when none reported a count.
    pub queries: Option<u32>,
    /// Some resolver in the group passed on the client subnet (EDNS Client Subnet).
    pub ecs: bool,
}

/// The resolvers grouped by owner, in the order first seen; an owner-less one is "Unknown network".
pub fn groups(report: &Report, lang: Lang) -> Vec<ResolverGroup> {
    let mut groups: Vec<ResolverGroup> = Vec::new();
    for resolver in &report.resolvers {
        let name = resolver
            .org
            .clone()
            .unwrap_or_else(|| tr_in(lang, "leak.unknown_network", &[]));
        let index = groups
            .iter()
            .position(|g| g.name == name)
            .unwrap_or_else(|| {
                groups.push(ResolverGroup {
                    name,
                    location: resolver.location(),
                    addresses: Vec::new(),
                    queries: None,
                    ecs: false,
                });
                groups.len() - 1
            });
        let group = &mut groups[index];
        group.addresses.push(resolver.ip.clone());
        if let Some(q) = resolver.queries {
            group.queries = Some(group.queries.unwrap_or(0) + q);
        }
        group.ecs |= resolver.subnet.is_some();
    }
    groups
}

/// The report as terminal text: your exit, then the resolvers grouped by owner.
pub fn format_report(report: &Report, lang: Lang) -> String {
    let t = |key: &str, args: &[(&str, &str)]| tr_in(lang, key, args);
    let dot = " \u{b7} ";
    let mut out = String::new();
    let you = [Some(report.you.name()), report.you.location()];
    out += &format!(
        "{}: {}\n",
        t("leak.you", &[]),
        you.into_iter().flatten().collect::<Vec<_>>().join(dot)
    );
    if report.resolvers.is_empty() {
        return out;
    }
    let groups = groups(report, lang);
    out += &format!("{}:\n", t("leak.resolvers", &[]));
    for group in &groups {
        let mut head = vec![group.name.clone()];
        head.extend(group.location.clone());
        head.extend(
            group
                .queries
                .map(|q| t("leak.queries", &[("count", &q.to_string())])),
        );
        out += &format!("  {}\n    {}\n", head.join(dot), group.addresses.join(", "));
    }
    if groups.iter().any(|g| g.addresses.len() > 1) {
        out += &format!("{}\n", t("leak.many_ips", &[]));
    }
    let orgs = report.passes_subnet();
    if !orgs.is_empty() {
        let orgs = orgs.join(&t("list_separator", &[]));
        out += &format!("{}\n", t("leak.subnet", &[("orgs", &orgs)]));
    }
    out
}

/// 16 lowercase hex digits from 64 random bits.
pub fn make_token() -> String {
    let id = uuid::Uuid::new_v4();
    let bytes = id.as_bytes();
    // Bytes 6 and 8 carry the fixed version and variant bits; skip them.
    [0, 1, 2, 3, 4, 5, 9, 10]
        .iter()
        .map(|&i| format!("{:02x}", bytes[i]))
        .collect()
}

/// `<token>-1.leak.cellglide.com` … `<token>-<count>.leak.cellglide.com`.
pub fn names(token: &str, count: usize) -> Vec<String> {
    (1..=count).map(|n| format!("{token}-{n}.{ZONE}")).collect()
}

pub fn result_path(token: &str) -> String {
    format!("/v1/{token}")
}

/// One name through the system resolver, both A and AAAA (as apps ask), giving up after a
/// few seconds; the answer itself doesn't matter, only that the query went out.
pub fn system_resolve(name: String) -> BoxFuture<'static, ()> {
    Box::pin(async move {
        let _ = tokio::time::timeout(RESOLVE_TIMEOUT, tokio::net::lookup_host((name, 0))).await;
    })
}

/// Resolves the 20 names through `resolve`, then fetches what the server saw.
pub async fn run(
    resolve: impl Fn(String) -> BoxFuture<'static, ()>,
    target: &Target,
) -> Result<Report, ToolError> {
    let token = make_token();
    join_all(names(&token, NAMES).into_iter().map(&resolve)).await;
    let (status, body) = https::get(
        target,
        &result_path(&token),
        &crate::user_agent(),
        FETCH_TIMEOUT,
    )
    .await?;
    if status != 200 {
        return Err(ToolError::Status(status));
    }
    serde_json::from_slice(&body).map_err(|_| ToolError::BadReply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::https::tests::fake_http;
    use std::sync::{Arc, Mutex};

    /// A server reply like the Mac tests build: `you` has an owner only when `you` has an ASN;
    /// the optional subnet goes on the first resolver.
    fn report(
        you: Option<u32>,
        resolvers: &[(&str, Option<u32>, Option<&str>)],
        subnet: Option<&str>,
    ) -> Report {
        let mut json = String::from(r#"{"v":1,"you":{"ip":"198.51.100.47""#);
        if let Some(asn) = you {
            json += &format!(
                r#","asn":{asn},"org":"AT&T Enterprises, LLC","country":"US","region":"California""#
            );
        }
        json += r#"},"resolvers":["#;
        let entries: Vec<String> = resolvers
            .iter()
            .enumerate()
            .map(|(index, (ip, asn, org))| {
                let mut entry = format!(r#"{{"ip":"{ip}","queries":3"#);
                if let Some(asn) = asn {
                    entry += &format!(r#","asn":{asn}"#);
                }
                if let Some(org) = org {
                    entry += &format!(r#","org":"{org}","country":"US","region":"Virginia""#);
                }
                if let (0, Some(subnet)) = (index, subnet) {
                    entry += &format!(r#","subnet":"{subnet}""#);
                }
                entry + "}"
            })
            .collect();
        json += &entries.join(",");
        json += "]}";
        serde_json::from_str(&json).unwrap()
    }

    const PHONE: Option<DnsMode> = Some(DnsMode::Phone);

    // DNSLeakTests.testDecodesTheServersReport
    #[test]
    fn decodes_the_servers_report() {
        let r = report(
            Some(7018),
            &[("2620:0:cc8::209", Some(36692), Some("Cisco OpenDNS, LLC"))],
            Some("198.51.100.0/24"),
        );
        assert_eq!(r.you.org.as_deref(), Some("AT&T Enterprises, LLC"));
        assert_eq!(r.resolvers[0].asn, Some(36692));
        assert_eq!(r.resolvers[0].subnet.as_deref(), Some("198.51.100.0/24"));
        assert_eq!(r.resolvers[0].queries, Some(3));
        assert_eq!(r.resolvers[0].location().as_deref(), Some("Virginia, US"));
        // Country alone, and nothing at all, when the server knows less.
        let bare = Network {
            country: Some("US".into()),
            region: Some(String::new()),
            ..Network::default()
        };
        assert_eq!(bare.location().as_deref(), Some("US"));
        assert_eq!(Network::default().location(), None);
    }

    // testOnlyEncryptedDNSProvidersIsNoLeak
    #[test]
    fn only_encrypted_dns_providers_is_no_leak() {
        let r = report(
            Some(7018),
            &[
                ("172.64.216.24", Some(13335), Some("Cloudflare, Inc.")),
                ("104.22.194.25", Some(13335), Some("Cloudflare, Inc.")),
            ],
            None,
        );
        assert_eq!(
            judge(&r, PHONE),
            Verdict::Encrypted {
                providers: vec!["Cloudflare".into()]
            }
        );
        // The phone's DNS being off (public resolvers through the tunnel) is judged the same.
        assert_eq!(
            judge(&r, Some(DnsMode::Public)),
            Verdict::Encrypted {
                providers: vec!["Cloudflare".into()]
            }
        );
    }

    // testTheExitNetworksOwnDNSMeansTheCarrierAnswered
    #[test]
    fn the_exit_networks_own_dns_means_the_carrier_answered() {
        let r = report(
            Some(7018),
            &[("12.127.16.1", Some(7018), Some("AT&T Enterprises, LLC"))],
            None,
        );
        assert_eq!(
            judge(&r, PHONE),
            Verdict::Carrier {
                network: "AT&T Enterprises, LLC".into()
            }
        );
    }

    // testAnUnknownResolverWhileTunnelingIsALeak
    #[test]
    fn an_unknown_resolver_while_tunneling_is_a_leak() {
        let r = report(
            Some(7018),
            &[
                ("172.64.216.24", Some(13335), Some("Cloudflare, Inc.")),
                (
                    "75.75.75.75",
                    Some(7922),
                    Some("Comcast Cable Communications, LLC"),
                ),
            ],
            None,
        );
        assert_eq!(
            judge(&r, PHONE),
            Verdict::Leak {
                networks: vec!["Comcast Cable Communications, LLC".into()]
            }
        );
    }

    // testWithoutTheTunnelItReportsTheMacsOwnNetwork
    #[test]
    fn without_the_phone_it_reports_the_computers_own_network() {
        let r = report(
            Some(7922),
            &[(
                "75.75.75.75",
                Some(7922),
                Some("Comcast Cable Communications, LLC"),
            )],
            None,
        );
        assert_eq!(judge(&r, None), Verdict::NotSharing);
    }

    // testNothingSeenMeansTheTestDidNotGetThrough
    #[test]
    fn nothing_seen_means_the_test_did_not_get_through() {
        let r = report(Some(7018), &[], None);
        assert_eq!(judge(&r, PHONE), Verdict::NothingSeen);
        // Nothing seen wins over every mode.
        assert_eq!(judge(&r, None), Verdict::NothingSeen);
    }

    // testAResolverWithoutKnownOwnerCountsAsUnknown
    #[test]
    fn a_resolver_without_known_owner_counts_as_unknown() {
        let r = report(Some(7018), &[("2a06:98c0:3600::103", None, None)], None);
        assert_eq!(
            judge(&r, PHONE),
            Verdict::Leak {
                networks: vec!["2a06:98c0:3600::103".into()]
            }
        );
    }

    // testNamesAreRandomAndShaped
    #[test]
    fn names_are_random_and_shaped() {
        let token = make_token();
        assert_eq!(token.len(), 16);
        assert_ne!(token, make_token());
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        );
        assert_eq!(
            names(&token, 3),
            vec![
                format!("{token}-1.leak.cellglide.com"),
                format!("{token}-2.leak.cellglide.com"),
                format!("{token}-3.leak.cellglide.com"),
            ]
        );
        assert_eq!(result_path(&token), format!("/v1/{token}"));
        let target = Target::leakcheck();
        assert_eq!(
            (target.host.as_str(), target.port, target.tls),
            ("leakcheck.cellglide.com", 443, true)
        );
        assert_eq!(
            (NAMES, ZONE, RESOLVE_TIMEOUT, FETCH_TIMEOUT),
            (
                20,
                "leak.cellglide.com",
                Duration::from_secs(6),
                Duration::from_secs(15)
            )
        );
    }

    // testSubnetsPassedOnAreListed
    #[test]
    fn subnets_passed_on_are_listed() {
        let r = report(
            Some(7018),
            &[("172.253.0.27", Some(15169), Some("Google LLC"))],
            Some("198.51.100.0/24"),
        );
        assert_eq!(r.passes_subnet(), vec!["Google LLC".to_string()]);
        assert!(
            report(Some(7018), &[("1.1.1.1", None, None)], None)
                .passes_subnet()
                .is_empty()
        );
    }

    // Linux only
    #[test]
    fn unmanaged_dns_lists_the_original_networks() {
        let r = report(
            Some(7018),
            &[
                (
                    "75.75.75.75",
                    Some(7922),
                    Some("Comcast Cable Communications, LLC"),
                ),
                ("172.64.216.24", Some(13335), Some("Cloudflare, Inc.")),
                (
                    "75.75.76.76",
                    Some(7922),
                    Some("Comcast Cable Communications, LLC"),
                ),
            ],
            None,
        );
        assert_eq!(
            judge(&r, Some(DnsMode::Unmanaged)),
            Verdict::Unmanaged {
                networks: vec![
                    "Comcast Cable Communications, LLC".into(),
                    "Cloudflare, Inc.".into()
                ]
            }
        );
        assert_eq!(
            judge(&report(Some(7018), &[], None), Some(DnsMode::Unmanaged)),
            Verdict::NothingSeen
        );
    }

    #[tokio::test]
    async fn run_resolves_every_name_then_fetches_the_token() {
        let body = br#"{"v":1,"you":{"ip":"1.2.3.4","asn":7018},"resolvers":[]}"#.to_vec();
        let (target, server) = fake_http("200 OK", body).await;
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let log = seen.clone();
        let report = run(
            move |name| {
                log.lock().unwrap().push(name);
                Box::pin(async {})
            },
            &target,
        )
        .await
        .unwrap();
        assert_eq!(report.you.ip, "1.2.3.4");
        assert_eq!(report.you.asn, Some(7018));
        assert!(report.resolvers.is_empty());

        let names = seen.lock().unwrap().clone();
        assert_eq!(names.len(), 20);
        let token = names[0].split('-').next().unwrap().to_string();
        assert_eq!(token.len(), 16);
        for (i, name) in names.iter().enumerate() {
            assert_eq!(*name, format!("{token}-{}.leak.cellglide.com", i + 1));
        }
        let head = server.await.unwrap().to_ascii_lowercase();
        assert!(head.starts_with(&format!("get /v1/{token} http/1.1")));
        assert!(head.contains("user-agent: cellglide-linux/"));
    }

    #[tokio::test]
    async fn non_200_is_service_unavailable() {
        let (target, _server) = fake_http("503 Service Unavailable", b"{}".to_vec()).await;
        assert_eq!(
            run(|_| Box::pin(async {}), &target).await,
            Err(ToolError::Status(503))
        );
    }

    #[tokio::test]
    async fn a_garbled_reply_is_bad() {
        let (target, _server) = fake_http("200 OK", b"<html>".to_vec()).await;
        assert_eq!(
            run(|_| Box::pin(async {}), &target).await,
            Err(ToolError::BadReply)
        );
    }

    #[tokio::test]
    async fn system_resolve_gives_up_instead_of_hanging() {
        // The answer is unused; this only has to come back (a refused or failed lookup is fine).
        let done = tokio::time::timeout(
            RESOLVE_TIMEOUT + Duration::from_secs(2),
            system_resolve("localhost".into()),
        )
        .await;
        assert!(done.is_ok());
    }

    #[test]
    fn ecs_networks_share_one_sentence() {
        let mut r = report(
            Some(7018),
            &[
                ("172.253.0.27", Some(15169), Some("Google LLC")),
                ("172.64.216.24", Some(13335), Some("Cloudflare, Inc.")),
            ],
            Some("198.51.100.0/24"),
        );
        r.resolvers[1].subnet = r.resolvers[0].subnet.clone();
        let text = format_report(&r, Lang::ZhCn);
        let lines: Vec<&str> = text.lines().filter(|l| l.contains("网段")).collect();
        assert_eq!(
            lines,
            vec![
                "Google LLC、Cloudflare, Inc. 会把你所在的网段告诉网站的 DNS 服务器，网站能据此知道你的大致位置。"
            ]
        );
        assert!(!text.contains("! "));
    }

    #[test]
    fn the_report_is_grouped_by_owner() {
        let r = report(
            Some(7018),
            &[
                ("172.64.216.24", Some(13335), Some("Cloudflare, Inc.")),
                ("172.64.216.25", Some(13335), Some("Cloudflare, Inc.")),
                ("2a06:98c0:3600::103", None, None),
            ],
            Some("198.51.100.0/24"),
        );
        let text = format_report(&r, Lang::En);
        assert!(text.starts_with("Your exit: AT&T Enterprises, LLC \u{b7} California, US\n"));
        assert!(text.contains("  Cloudflare, Inc. \u{b7} Virginia, US \u{b7} 6 queries\n    172.64.216.24, 172.64.216.25\n"));
        assert!(text.contains("  Unknown network \u{b7} 3 queries\n    2a06:98c0:3600::103\n"));
        assert!(text.contains("one network can show several IPs"));
        assert!(text.contains("Cloudflare, Inc. tells websites’ DNS servers"));
        assert!(!text.contains("! "));
        // No resolvers: just the exit.
        let none = format_report(&report(Some(7018), &[], None), Lang::ZhCn);
        assert_eq!(
            none,
            "你的出口: AT&T Enterprises, LLC \u{b7} California, US\n"
        );
    }

    #[test]
    fn groups_merge_one_network_and_keep_first_location() {
        let r = report(
            Some(7018),
            &[
                ("1.1.1.1", Some(13335), Some("Cloudflare")),
                ("1.0.0.1", Some(13335), Some("Cloudflare")),
                ("9.9.9.9", None, None),
            ],
            Some("203.0.113.0/24"),
        );
        let groups = groups(&r, Lang::En);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].name, "Cloudflare");
        assert_eq!(groups[0].location.as_deref(), Some("Virginia, US"));
        assert_eq!(groups[0].addresses, ["1.1.1.1", "1.0.0.1"]);
        assert_eq!(groups[0].queries, Some(6));
        assert!(groups[0].ecs);
        assert_eq!(groups[1].name, "Unknown network");
        assert_eq!(groups[1].location, None);
        assert!(!groups[1].ecs);
    }

    #[test]
    fn every_verdict_has_text_in_both_languages() {
        let verdicts = Verdict::all();
        let others = [
            "leak.running",
            "leak.about",
            "leak.you",
            "leak.resolvers",
            "leak.unknown_network",
            "leak.many_ips",
            "leak.queries",
            "leak.subnet",
            "leak.unavailable",
            "leak.failed",
        ];
        for key in others {
            assert!(cg_i18n::KEYS.contains(&key), "{key} missing from KEYS");
            let zh = tr_in(Lang::ZhCn, key, &[]);
            let en = tr_in(Lang::En, key, &[]);
            assert_ne!(zh, key, "zh-CN missing {key}");
            assert_ne!(en, key, "en missing {key}");
            assert_ne!(zh, en, "{key} not translated");
        }
        for verdict in &verdicts {
            for part in ["title", "detail"] {
                let key = format!("leak.{}.{part}", verdict.key_name());
                assert!(
                    cg_i18n::KEYS.contains(&key.as_str()),
                    "{key} missing from KEYS"
                );
            }
        }
        for verdict in &verdicts {
            let (zh_title, zh_detail) = describe(verdict, Lang::ZhCn);
            let (en_title, en_detail) = describe(verdict, Lang::En);
            for text in [&zh_title, &zh_detail, &en_title, &en_detail] {
                assert!(!text.is_empty() && !text.contains("leak."), "{text}");
                assert!(!text.contains("%{"), "unfilled placeholder in {text}");
            }
            assert_ne!(zh_title, en_title);
            assert_ne!(zh_detail, en_detail);
        }
        // The arguments reach the sentence, in each language's list style.
        assert!(
            describe(&verdicts[0], Lang::ZhCn)
                .1
                .contains("Cloudflare、Google")
        );
        assert!(
            describe(&verdicts[0], Lang::En)
                .1
                .contains("Cloudflare, Google")
        );
        assert!(describe(&verdicts[1], Lang::En).1.contains("CARRIER"));
        assert!(
            describe(&verdicts[2], Lang::ZhCn)
                .1
                .contains("NET-A、NET-B")
        );
        assert!(describe(&verdicts[5], Lang::En).1.contains("NET-A"));
    }
}
