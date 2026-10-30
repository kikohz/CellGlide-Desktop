//! The DNS leak test page, after the Mac's `DNSLeakPage`: which DNS servers this computer's
//! lookups really reach, and whether that is what the connection set up. Runs only when the
//! user clicks, in this process (never in the service). When the system wouldn't let CellGlide
//! manage DNS, the verdict says so and the page can run `doctor` in place.

use super::Ctx;
use crate::effect::{DoctorFor, Effect};
use crate::widgets::checks::{self, Run};
use crate::widgets::kit::{self, Tone, ToneColor};
use cg_i18n::{Lang, tr};
use cg_ipc::DnsMode;
use cg_tools::dns_leak::view::{self as leak_view, start_label, subnet_line, tone, you_line};
use cg_tools::dns_leak::{self, Report, Verdict};
use iced::widget::{button, column, container, row, text};
use iced::{Alignment, Background, Border, Element, Font, Length, Theme};

#[derive(Debug, Default)]
pub struct State {
    running: bool,
    result: Option<(Report, Verdict)>,
    failure: Option<String>,
    /// `doctor`'s answer, for the advice when DNS isn't the phone's.
    doctor: Run,
    /// The DNS mode the last run started with: `Unmanaged` explains itself even when the
    /// resolvers seen did not decide the verdict.
    sharing: Option<DnsMode>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Start,
    Done(Result<(Report, Verdict), String>),
    RunChecks,
    /// `doctor`'s checks, for the advice when DNS isn't the phone's.
    /// The token of the ask it answers (`checks::Run`).
    Checks(u64, Result<Vec<cg_ipc::Check>, String>),
    /// Copy a fix from the checks.
    Copy(String),
    OpenDbIp,
}

pub fn update(state: &mut State, message: Msg, ctx: &Ctx) -> Effect {
    match message {
        Msg::Start if !state.running => {
            state.running = true;
            state.result = None;
            state.failure = None;
            state.doctor.clear();
            state.sharing = ctx.dns();
            Effect::StartLeak {
                sharing: state.sharing,
            }
        }
        Msg::Start => Effect::None,
        Msg::Done(result) => {
            state.running = false;
            match result {
                Ok(done) => state.result = Some(done),
                Err(reason) => state.failure = Some(reason),
            }
            Effect::None
        }
        Msg::RunChecks if offers_checks(state) => state.doctor.start(DoctorFor::Leak),
        Msg::RunChecks => Effect::None,
        Msg::Checks(token, result) => {
            state.doctor.finish(token, result);
            Effect::None
        }
        Msg::Copy(fix) => Effect::Copy(fix),
        Msg::OpenDbIp => Effect::OpenUrl("https://db-ip.com".into()),
    }
}

/// Whether the checks are offered under the verdict (`leak_view::offers_checks`).
pub fn offers_checks(state: &State) -> bool {
    leak_view::offers_checks(state.result.as_ref().map(|(_, v)| v), state.sharing)
}

fn coloured(content: String, size: u32, tone: Tone) -> text::Text<'static> {
    text(content).size(size).color(tone.color())
}

fn verdict_block<'a>(verdict: &Verdict) -> Element<'a, Msg> {
    let (title, detail) = dns_leak::describe(verdict, cg_i18n::current());
    let tone = tone(verdict);
    container(
        column![
            coloured(title, 15, tone).font(kit::SEMIBOLD),
            kit::secondary(detail).size(13)
        ]
        .spacing(3),
    )
    .padding(14)
    .width(Length::Fill)
    .style(move |_: &Theme| container::Style {
        background: Some(Background::Color(tone.color().scale_alpha(0.12))),
        border: Border {
            radius: 10.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    })
    .into()
}

fn servers_card<'a>(report: &Report, lang: Lang) -> Element<'a, Msg> {
    let groups = dns_leak::groups(report, lang);
    let mut list = column![].spacing(8);
    if groups.is_empty() {
        list = list.push(kit::secondary(tr("leak.none_seen", &[])).size(13));
    }
    for group in &groups {
        list = list.push(
            row![
                text(group.name.clone())
                    .size(13)
                    .width(Length::FillPortion(3)),
                text(group.addresses.join("\n"))
                    .font(Font::MONOSPACE)
                    .size(12)
                    .width(Length::FillPortion(4)),
                kit::secondary(group.location.clone().unwrap_or_default())
                    .width(Length::FillPortion(2))
                    .align_x(iced::alignment::Horizontal::Right),
            ]
            .spacing(12),
        );
    }
    let mut body = column![
        kit::card_row(tr("leak.you", &[]), None, text(you_line(report)).size(13)),
        kit::secondary(tr("leak.resolvers", &[])).size(13),
        list,
    ]
    .spacing(10);
    if let Some(line) = subnet_line(&groups) {
        body = body.push(kit::secondary(line));
    }
    kit::card(None, body)
}

pub fn view<'a>(state: &'a State, _ctx: &Ctx<'a>) -> Element<'a, Msg> {
    let mut start = button(text(start_label(state.result.is_some())).size(13)).padding([5, 14]);
    if !state.running {
        start = start.on_press(Msg::Start);
    }
    let mut controls = row![start].spacing(10).align_y(Alignment::Center);
    if state.running {
        controls = controls.push(kit::secondary(tr("leak.running", &[])).size(13));
    }
    let mut body = column![kit::secondary(tr("leak.about", &[])).size(13), controls].spacing(14);
    if let Some(failure) = &state.failure {
        body = body.push(coloured(failure.clone(), 13, Tone::Red));
    }
    if let Some((report, verdict)) = &state.result {
        body = body.push(verdict_block(verdict));
        if offers_checks(state) {
            if !state.doctor.checking {
                body = body.push(
                    button(text(tr("leak.run_checks", &[])).size(13))
                        .padding([4, 12])
                        .on_press(Msg::RunChecks),
                );
            }
            if let Some(list) = checks::run_view(&state.doctor, None, Msg::Copy) {
                body = body.push(list);
            }
        }
        body = body
            .push(servers_card(report, cg_i18n::current()))
            .push(kit::secondary(tr("leak.many_ips", &[])).size(12));
    }
    body = body.push(
        button(kit::secondary(tr("leak.attribution", &[])))
            .padding(0)
            .style(button::text)
            .on_press(Msg::OpenDbIp),
    );
    kit::page(tr("nav.dns_leak", &[]), body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::fixtures::{connected_snapshot, disconnected_snapshot};
    use cg_ipc::{Check, CheckId, ConnState, Request};

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

    fn unmanaged_snapshot() -> cg_ipc::Snapshot {
        let mut snapshot = connected_snapshot();
        snapshot.state = ConnState::Connected {
            phone_version: "2.4.0".into(),
            dns: DnsMode::Unmanaged,
        };
        snapshot
    }

    fn unmanaged_verdict() -> Verdict {
        Verdict::Unmanaged {
            networks: vec!["AT&T".into()],
        }
    }

    #[test]
    fn start_passes_the_connection_dns_mode() {
        let mut state = State::default();
        let ctx = Ctx::for_test(Some(unmanaged_snapshot()));
        assert_eq!(
            update(&mut state, Msg::Start, &ctx),
            Effect::StartLeak {
                sharing: Some(DnsMode::Unmanaged)
            }
        );
        assert!(state.running);

        let mut state = State::default();
        let ctx = Ctx::for_test(Some(disconnected_snapshot()));
        assert_eq!(
            update(&mut state, Msg::Start, &ctx),
            Effect::StartLeak { sharing: None }
        );
        assert!(state.running);
        // Never two runs at once.
        assert_eq!(update(&mut state, Msg::Start, &ctx), Effect::None);
    }

    #[test]
    fn unmanaged_verdict_offers_the_checks() {
        let ctx = Ctx::for_test(Some(disconnected_snapshot()));
        let mut state = State::default();
        let _ = update(&mut state, Msg::Start, &ctx);
        // Not offered before there is a verdict.
        assert_eq!(update(&mut state, Msg::RunChecks, &ctx), Effect::None);
        let done = (report(ONE_RESOLVER), unmanaged_verdict());
        let _ = update(&mut state, Msg::Done(Ok(done)), &ctx);
        assert!(!state.running);
        assert!(offers_checks(&state));
        assert_eq!(
            update(&mut state, Msg::RunChecks, &ctx),
            Effect::Request(
                Request::Doctor,
                Reply::Doctor(DoctorFor::Leak, state.doctor.token)
            )
        );
        assert!(state.doctor.checking);
        let token = state.doctor.token;
        // An answer to some earlier ask is ignored.
        let _ = update(&mut state, Msg::Checks(token - 1, Err("old".into())), &ctx);
        assert!(state.doctor.checking);
        let _ = update(&mut state, Msg::Checks(token, Err("x".into())), &ctx);
        assert!(!state.doctor.checking);
        assert_eq!(state.doctor.checks, Some(Err("x".into())));
        let ok = vec![Check::new(CheckId::Tun, true)];
        let _ = update(&mut state, Msg::RunChecks, &ctx);
        let token = state.doctor.token;
        let _ = update(&mut state, Msg::Checks(token, Ok(ok.clone())), &ctx);
        assert_eq!(state.doctor.checks, Some(Ok(ok)));
    }

    use crate::effect::Reply;

    #[test]
    fn checks_are_offered_only_when_dns_is_unmanaged() {
        let mut state = State::default();
        let ctx = Ctx::for_test(Some(unmanaged_snapshot()));
        let _ = update(&mut state, Msg::Start, &ctx);
        let encrypted = Verdict::Encrypted {
            providers: vec!["Quad9".into()],
        };
        // Connected unmanaged before the test: offered whatever the resolvers said.
        let _ = update(
            &mut state,
            Msg::Done(Ok((report(ONE_RESOLVER), encrypted.clone()))),
            &ctx,
        );
        assert!(offers_checks(&state));

        let mut state = State::default();
        let ctx = Ctx::for_test(Some(connected_snapshot()));
        let _ = update(&mut state, Msg::Start, &ctx);
        let _ = update(
            &mut state,
            Msg::Done(Ok((report(ONE_RESOLVER), encrypted))),
            &ctx,
        );
        assert!(!offers_checks(&state));
        assert_eq!(update(&mut state, Msg::RunChecks, &ctx), Effect::None);
        assert!(!state.doctor.checking);
    }

    #[test]
    fn a_new_run_forgets_the_old_result() {
        let ctx = Ctx::for_test(Some(disconnected_snapshot()));
        let mut state = State::default();
        let _ = update(&mut state, Msg::Start, &ctx);
        let done = (report(ONE_RESOLVER), unmanaged_verdict());
        let _ = update(&mut state, Msg::Done(Ok(done)), &ctx);
        let _ = update(&mut state, Msg::RunChecks, &ctx);
        let token = state.doctor.token;
        let _ = update(&mut state, Msg::Checks(token, Err("x".into())), &ctx);
        let _ = update(&mut state, Msg::Done(Err("late".into())), &ctx);
        assert!(state.failure.is_some());

        let effect = update(&mut state, Msg::Start, &ctx);
        assert!(matches!(effect, Effect::StartLeak { .. }));
        assert!(state.running);
        assert!(state.result.is_none());
        assert!(state.failure.is_none());
        assert_eq!(state.doctor.checks, None);
        assert!(!state.doctor.checking);
    }

    #[test]
    fn a_failed_run_ends_with_its_reason() {
        let ctx = Ctx::for_test(None);
        let mut state = State::default();
        let _ = update(&mut state, Msg::Start, &ctx);
        let _ = update(&mut state, Msg::Done(Err("boom".into())), &ctx);
        assert!(!state.running);
        assert_eq!(state.failure.as_deref(), Some("boom"));
        assert!(state.result.is_none());
    }

    #[test]
    fn attribution_opens_db_ip_and_fixes_copy() {
        let ctx = Ctx::for_test(None);
        let mut state = State::default();
        assert_eq!(
            update(&mut state, Msg::OpenDbIp, &ctx),
            Effect::OpenUrl("https://db-ip.com".into())
        );
        assert_eq!(
            update(&mut state, Msg::Copy("sudo x".into()), &ctx),
            Effect::Copy("sudo x".into())
        );
    }

    #[test]
    fn the_view_builds_in_every_state() {
        let _lang = en();
        let ctx = Ctx::for_test(Some(unmanaged_snapshot()));
        let mut state = State::default();
        let _ = view(&state, &ctx);
        let _ = update(&mut state, Msg::Start, &ctx);
        let _ = view(&state, &ctx);
        let _ = update(&mut state, Msg::Done(Err("boom".into())), &ctx);
        let _ = view(&state, &ctx);
        let done = (report(ONE_RESOLVER), unmanaged_verdict());
        let _ = update(&mut state, Msg::Done(Ok(done)), &ctx);
        let _ = view(&state, &ctx);
        let _ = update(&mut state, Msg::RunChecks, &ctx);
        let _ = view(&state, &ctx);
        let checks = vec![Check::new(CheckId::Tun, false)];
        let token = state.doctor.token;
        let _ = update(&mut state, Msg::Checks(token, Ok(checks)), &ctx);
        let _ = view(&state, &ctx);
        let _ = update(&mut state, Msg::RunChecks, &ctx);
        let token = state.doctor.token;
        let _ = update(&mut state, Msg::Checks(token, Err("x".into())), &ctx);
        let _ = view(&state, &ctx);
        // No resolver at all: "None received".
        state.result = Some((
            report(r#"{"you":{"ip":"1.2.3.4"},"resolvers":[]}"#),
            Verdict::NothingSeen,
        ));
        let _ = view(&state, &ctx);
    }
}
