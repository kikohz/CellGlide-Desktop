//! The DNS leak page's words and colours, shared by the Linux and Windows windows: the verdict's
//! tone, the exit line, the client-subnet sentence, a failed run's sentence, and the run itself
//! as the page keeps it.

use super::{Report, ResolverGroup, Verdict};
use crate::ToolError;
use crate::https::Target;
use cg_i18n::tr;
use cg_ipc::DnsMode;
use cg_ipc::describe::ui::Tone;
use futures::future::BoxFuture;

pub fn tone(verdict: &Verdict) -> Tone {
    match verdict {
        Verdict::Encrypted { .. } => Tone::Green,
        Verdict::Carrier { .. } | Verdict::Unmanaged { .. } => Tone::Orange,
        Verdict::Leak { .. } => Tone::Red,
        Verdict::NotSharing | Verdict::NothingSeen => Tone::Gray,
    }
}

/// "AT&T Enterprises, LLC · California, US".
pub fn you_line(report: &Report) -> String {
    let mut parts = vec![report.you.org.clone().unwrap_or(report.you.ip.clone())];
    parts.extend(report.you.location());
    parts.join(" · ")
}

/// The sentence about resolvers that pass on the client subnet; none when no group does.
pub fn subnet_line(groups: &[ResolverGroup]) -> Option<String> {
    let names: Vec<&str> = groups
        .iter()
        .filter(|g| g.ecs)
        .map(|g| g.name.as_str())
        .collect();
    if names.is_empty() {
        return None;
    }
    Some(tr(
        "leak.subnet",
        &[("orgs", &names.join(&tr("list_separator", &[])))],
    ))
}

/// What a failed run says: an unavailable server, or the technical reason.
pub fn failure_text(error: &ToolError) -> String {
    match error {
        ToolError::Status(_) => tr("leak.unavailable", &[]),
        other => tr("leak.failed", &[("error", &other.to_string())]),
    }
}

/// A finished run as the page keeps it.
pub fn outcome(
    result: Result<Report, ToolError>,
    sharing: Option<DnsMode>,
) -> Result<(Report, Verdict), String> {
    match result {
        Ok(report) => {
            let verdict = super::judge(&report, sharing);
            Ok((report, verdict))
        }
        Err(error) => Err(failure_text(&error)),
    }
}

/// The test with the lookups and the server passed in, so tests never touch the network.
pub async fn run_with(
    resolve: impl Fn(String) -> BoxFuture<'static, ()>,
    target: Target,
    sharing: Option<DnsMode>,
) -> Result<(Report, Verdict), String> {
    outcome(super::run(resolve, &target).await, sharing)
}

/// The real test: the system resolver and the CellGlide server.
pub async fn run_production(sharing: Option<DnsMode>) -> Result<(Report, Verdict), String> {
    run_with(super::system_resolve, Target::leakcheck(), sharing).await
}

/// "Start test" before the first result, "Test again" after.
pub fn start_label(has_result: bool) -> String {
    tr(
        if has_result {
            "leak.again"
        } else {
            "leak.start"
        },
        &[],
    )
}

/// The checks are offered under the verdict when DNS isn't managed: the verdict says so, or
/// the connection was unmanaged when the test started (`sharing`).
pub fn offers_checks(verdict: Option<&Verdict>, sharing: Option<DnsMode>) -> bool {
    verdict.is_some()
        && (matches!(verdict, Some(Verdict::Unmanaged { .. }))
            || sharing == Some(DnsMode::Unmanaged))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cg_i18n::Lang;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn en() -> std::sync::MutexGuard<'static, ()> {
        let guard = cg_i18n::lock_for_tests();
        cg_i18n::set(Lang::En);
        guard
    }

    fn report(json: &str) -> Report {
        serde_json::from_str(json).unwrap()
    }

    const ONE_RESOLVER: &str = r#"{"you":{"ip":"1.2.3.4","org":"AT&T","country":"US","region":"California"},
        "resolvers":[{"ip":"9.9.9.9","asn":19281,"org":"Quad9","country":"US","queries":3,"subnet":"1.2.3.0/24"},
                     {"ip":"9.9.9.10","asn":19281,"org":"Quad9","country":"US","queries":2}]}"#;

    fn unmanaged_verdict() -> Verdict {
        Verdict::Unmanaged {
            networks: vec!["AT&T".into()],
        }
    }

    #[test]
    fn the_start_button_says_again_after_a_result() {
        let _lang = en();
        assert_eq!(start_label(false), "Start test");
        assert_eq!(start_label(true), "Test again");
    }

    #[test]
    fn checks_are_offered_only_when_dns_is_unmanaged() {
        let encrypted = Verdict::Encrypted {
            providers: vec!["Quad9".into()],
        };
        let unmanaged = unmanaged_verdict();
        // No verdict yet: never.
        assert!(!offers_checks(None, Some(DnsMode::Unmanaged)));
        assert!(!offers_checks(None, None));
        // The verdict says DNS isn't managed.
        assert!(offers_checks(Some(&unmanaged), None));
        // Connected unmanaged before the test: offered whatever the resolvers said.
        assert!(offers_checks(Some(&encrypted), Some(DnsMode::Unmanaged)));
        // Otherwise not.
        for sharing in [None, Some(DnsMode::Phone), Some(DnsMode::Public)] {
            assert!(!offers_checks(Some(&encrypted), sharing), "{sharing:?}");
        }
    }

    #[test]
    fn verdicts_have_their_colours() {
        assert_eq!(tone(&Verdict::Encrypted { providers: vec![] }), Tone::Green);
        assert_eq!(
            tone(&Verdict::Carrier {
                network: "x".into()
            }),
            Tone::Orange
        );
        assert_eq!(tone(&Verdict::Leak { networks: vec![] }), Tone::Red);
        assert_eq!(tone(&Verdict::NotSharing), Tone::Gray);
        assert_eq!(tone(&Verdict::NothingSeen), Tone::Gray);
        assert_eq!(tone(&unmanaged_verdict()), Tone::Orange);
    }

    #[test]
    fn the_exit_line_and_the_subnet_sentence() {
        let _lang = en();
        let r = report(ONE_RESOLVER);
        assert_eq!(you_line(&r), "AT&T · California, US");
        let bare = report(r#"{"you":{"ip":"1.2.3.4"},"resolvers":[]}"#);
        assert_eq!(you_line(&bare), "1.2.3.4");

        let groups = super::super::groups(&r, Lang::En);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].addresses, ["9.9.9.9", "9.9.9.10"]);
        let line = subnet_line(&groups).expect("Quad9 passes the subnet");
        assert!(line.starts_with("Quad9 tells"), "{line}");

        let none =
            report(r#"{"you":{"ip":"1.2.3.4"},"resolvers":[{"ip":"9.9.9.9","org":"Quad9"}]}"#);
        assert_eq!(subnet_line(&super::super::groups(&none, Lang::En)), None);
    }

    #[test]
    fn failures_are_unavailable_for_a_status_and_failed_otherwise() {
        let _lang = en();
        assert_eq!(
            failure_text(&ToolError::Status(503)),
            tr("leak.unavailable", &[])
        );
        let offline = failure_text(&ToolError::Offline("no route".into()));
        assert!(offline.starts_with("The test did not finish"), "{offline}");
        assert!(offline.contains("no route"));
    }

    #[test]
    fn outcome_judges_a_report_and_words_a_failure() {
        let (report, verdict) = outcome(Ok(report(ONE_RESOLVER)), Some(DnsMode::Unmanaged))
            .expect("a report is a result");
        assert_eq!(report.resolvers.len(), 2);
        assert!(matches!(verdict, Verdict::Unmanaged { .. }));
        let _lang = en();
        assert_eq!(
            outcome(Err(ToolError::Status(503)), None),
            Err(tr("leak.unavailable", &[]))
        );
    }

    /// A one-shot HTTP server on localhost answering with `status` and `body`; the paths it
    /// was asked for are recorded.
    async fn fake_server(status: u16, body: &'static str) -> (Target, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let paths = Arc::new(Mutex::new(Vec::new()));
        let seen = paths.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut buffer = vec![0u8; 4096];
                let n = stream.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..n]).to_string();
                if let Some(path) = request.split_whitespace().nth(1) {
                    seen.lock().unwrap().push(path.to_string());
                }
                let reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            }
        });
        (
            Target {
                host: "127.0.0.1".into(),
                port,
                tls: false,
            },
            paths,
        )
    }

    fn fake_resolve(asked: Arc<Mutex<Vec<String>>>) -> impl Fn(String) -> BoxFuture<'static, ()> {
        move |name| {
            let asked = asked.clone();
            Box::pin(async move { asked.lock().unwrap().push(name) })
        }
    }

    #[tokio::test]
    async fn the_engine_judges_with_the_connection_dns_mode() {
        let (target, paths) = fake_server(200, ONE_RESOLVER).await;
        let asked = Arc::new(Mutex::new(Vec::new()));
        let result = run_with(
            fake_resolve(asked.clone()),
            target,
            Some(DnsMode::Unmanaged),
        )
        .await
        .expect("the fake server answers");
        assert!(matches!(result.1, Verdict::Unmanaged { .. }));
        assert_eq!(result.0.resolvers.len(), 2);
        assert_eq!(asked.lock().unwrap().len(), super::super::NAMES);
        assert!(paths.lock().unwrap()[0].starts_with("/v1/"));
    }

    #[tokio::test]
    async fn a_server_error_is_the_unavailable_sentence() {
        let (target, _) = fake_server(503, "{}").await;
        let asked = Arc::new(Mutex::new(Vec::new()));
        let error = run_with(fake_resolve(asked), target, None)
            .await
            .unwrap_err();
        // Other tests switch the language meanwhile; either language's sentence will do.
        assert!(
            [Lang::ZhCn, Lang::En]
                .iter()
                .any(|&lang| error == cg_i18n::tr_in(lang, "leak.unavailable", &[])),
            "{error}"
        );
    }

    #[tokio::test]
    async fn an_unreachable_server_is_the_failed_sentence() {
        // Nothing listens here once the listener is dropped.
        let port = {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap().port()
        };
        let target = Target {
            host: "127.0.0.1".into(),
            port,
            tls: false,
        };
        let asked = Arc::new(Mutex::new(Vec::new()));
        let error = run_with(fake_resolve(asked), target, None)
            .await
            .unwrap_err();
        assert!(
            error.starts_with("The test did not finish") || error.starts_with("检测没有完成"),
            "{error}"
        );
    }
}
