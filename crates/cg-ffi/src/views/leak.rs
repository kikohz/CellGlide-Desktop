//! The DNS leak test page: the start button, the verdict with its colour and explanation, your
//! exit, the resolvers grouped by owner, and a failed run's sentence.

use cg_i18n::current;
use cg_ipc::DnsMode;
use cg_ipc::describe::ui;
use cg_tools::ToolError;
use cg_tools::dns_leak::view::{
    failure_text, offers_checks, start_label, subnet_line, tone, you_line,
};
use cg_tools::dns_leak::{Report, ResolverGroup, Verdict, describe, groups};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct LeakView {
    pub running: bool,
    /// "Start test", or "Test again" after a result.
    pub start_label: String,
    /// Offer the checks under the verdict: DNS isn't managed.
    pub offers_checks: bool,
    pub result: Option<LeakResultView>,
    /// Why the last run gave no result.
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LeakResultView {
    pub tone: ui::Tone,
    pub title: String,
    pub detail: String,
    /// "AT&T Enterprises, LLC · California, US".
    pub you: String,
    pub groups: Vec<ResolverGroupView>,
    /// Which resolvers pass on the client subnet; none when no group does.
    pub subnet: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ResolverGroupView {
    pub name: String,
    pub location: Option<String>,
    pub addresses: Vec<String>,
    pub queries: Option<u32>,
    /// Passes on the client subnet.
    pub ecs: bool,
}

/// How the last run ended: what the server saw and the verdict, or why there's nothing.
pub(crate) type LeakOutcome = Result<(Report, Verdict), ToolError>;

/// The page, in the current language. `sharing` is how DNS was handled when the test started.
pub(crate) fn leak(
    running: bool,
    outcome: Option<&LeakOutcome>,
    sharing: Option<DnsMode>,
) -> LeakView {
    let verdict = match outcome {
        Some(Ok((_, verdict))) => Some(verdict),
        _ => None,
    };
    LeakView {
        running,
        start_label: start_label(verdict.is_some()),
        offers_checks: offers_checks(verdict, sharing),
        result: match outcome {
            Some(Ok((report, verdict))) => Some(result(report, verdict)),
            _ => None,
        },
        error: match outcome {
            Some(Err(error)) => Some(failure_text(error)),
            _ => None,
        },
    }
}

fn result(report: &Report, verdict: &Verdict) -> LeakResultView {
    let lang = current();
    let (title, detail) = describe(verdict, lang);
    let grouped = groups(report, lang);
    LeakResultView {
        tone: tone(verdict),
        title,
        detail,
        you: you_line(report),
        subnet: subnet_line(&grouped),
        groups: grouped.into_iter().map(group).collect(),
    }
}

fn group(group: ResolverGroup) -> ResolverGroupView {
    ResolverGroupView {
        name: group.name,
        location: group.location,
        addresses: group.addresses,
        queries: group.queries,
        ecs: group.ecs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cg_i18n::{Lang, tr};
    use cg_tools::dns_leak::judge;

    const ONE_RESOLVER: &str = r#"{"you":{"ip":"1.2.3.4","org":"AT&T","country":"US","region":"California"},
        "resolvers":[{"ip":"9.9.9.9","asn":19281,"org":"Quad9","country":"US","queries":3,"subnet":"1.2.3.0/24"},
                     {"ip":"9.9.9.10","asn":19281,"org":"Quad9","country":"US","queries":2}]}"#;

    #[test]
    fn a_result_is_the_verdict_in_words_and_the_resolvers_grouped() {
        let _g = cg_i18n::lock_for_tests();
        cg_i18n::set(Lang::En);
        let report: Report = serde_json::from_str(ONE_RESOLVER).unwrap();
        let verdict = judge(&report, Some(DnsMode::Unmanaged));
        let outcome = Ok((report.clone(), verdict.clone()));
        let view = leak(false, Some(&outcome), Some(DnsMode::Unmanaged));
        assert_eq!(view.start_label, tr("leak.again", &[]));
        assert!(view.offers_checks);
        assert_eq!(view.error, None);
        let result = view.result.clone().unwrap();
        assert_eq!((result.title, result.detail), describe(&verdict, Lang::En));
        assert_eq!(result.tone, tone(&verdict));
        assert_eq!(result.you, "AT&T · California, US");
        assert_eq!(result.groups.len(), 1);
        assert_eq!(result.groups[0].addresses, ["9.9.9.9", "9.9.9.10"]);
        assert_eq!(result.groups[0].queries, Some(5));
        assert!(result.groups[0].ecs);
        assert!(result.subnet.unwrap().contains("Quad9"));
        crate::testing::tuple_free(&view).unwrap();
        // The same outcome, after a language change, reads in the new language.
        cg_i18n::set(Lang::ZhCn);
        let again = leak(false, Some(&outcome), Some(DnsMode::Unmanaged));
        assert_eq!(again.start_label, tr("leak.again", &[]));
        assert_ne!(again, view);
        cg_i18n::set(Lang::En);
    }

    #[test]
    fn before_a_result_and_after_a_failure() {
        let _g = cg_i18n::lock_for_tests();
        cg_i18n::set(Lang::En);
        let idle = leak(false, None, None);
        assert_eq!(idle.start_label, tr("leak.start", &[]));
        assert!(!idle.offers_checks && idle.result.is_none() && idle.error.is_none());
        let failed = Err(ToolError::Status(503));
        let view = leak(false, Some(&failed), None);
        assert_eq!(view.error, Some(tr("leak.unavailable", &[])));
        assert_eq!(view.start_label, tr("leak.start", &[]));
        assert!(leak(true, None, None).running);
    }
}
