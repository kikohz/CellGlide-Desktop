//! Requests to the service and where their replies go. Every failure, including a reply of the
//! wrong kind from a service of another version, reaches the page as text, so no page is left
//! waiting.

use crate::app::Message;
use crate::effect::{DoctorFor, Reply};
use crate::pages::{PageMsg, diagnostics, history, leak, settings};
use cg_i18n::tr;
use cg_ipc::{Request, Response, client, describe};
use std::path::Path;
use std::time::Duration;

/// How long a reply may take before the page stops waiting.
const TIMEOUT: Duration = Duration::from_secs(10);
/// The diagnostics bundle reads the journal, which can take a while.
const DIAGNOSTICS_TIMEOUT: Duration = Duration::from_secs(90);

fn timeout_for(request: &Request) -> Duration {
    match request {
        Request::Diagnostics { .. } => DIAGNOSTICS_TIMEOUT,
        _ => TIMEOUT,
    }
}

/// One request; the service's own refusal, an unreachable service and one that never answers
/// all become text.
pub async fn call(request: Request) -> Result<Response, String> {
    call_at(Path::new(cg_ipc::SOCKET_PATH), request).await
}

async fn call_at(path: &Path, request: Request) -> Result<Response, String> {
    match tokio::time::timeout(timeout_for(&request), client::request(path, &request)).await {
        Err(_elapsed) => Err(tr("gui.service_timeout", &[])),
        Ok(Ok(Response::Error { reason })) => Err(describe::reason_text(&reason)),
        Ok(Ok(response)) => Ok(response),
        Ok(Err(error)) => Err(describe::unreachable(&error)),
    }
}

fn unexpected() -> String {
    tr("cli.unexpected_reply", &[])
}

/// Hands a reply to the page that asked for it.
pub fn route(reply: Reply, result: Result<Response, String>) -> Message {
    let page = match reply {
        Reply::History => PageMsg::History(history::Msg::Loaded(result.and_then(|r| match r {
            Response::History { day } => Ok(day),
            _ => Err(unexpected()),
        }))),
        Reply::Diagnostics => {
            PageMsg::Diagnostics(diagnostics::Msg::Prepared(result.and_then(|r| match r {
                Response::Diagnostics { json } => Ok(json),
                _ => Err(unexpected()),
            })))
        }
        Reply::Settings(field) => PageMsg::Settings(settings::Msg::Saved(
            field,
            result.and_then(|r| match r {
                Response::Ok => Ok(()),
                _ => Err(unexpected()),
            }),
        )),
        Reply::Doctor(for_page, token) => {
            let checks = result.and_then(|r| match r {
                Response::Doctor { checks } => Ok(checks),
                _ => Err(unexpected()),
            });
            match for_page {
                DoctorFor::Settings => PageMsg::Settings(settings::Msg::Checks(token, checks)),
                DoctorFor::Leak => PageMsg::Leak(leak::Msg::Checks(token, checks)),
            }
        }
    };
    Message::Page(page)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::SettingField;
    use crate::pages::{PageMsg, diagnostics, history, leak, settings};

    #[test]
    fn request_failure_reaches_the_page_as_text() {
        assert!(matches!(
            route(Reply::History, Err("服务已停止".into())),
            Message::Page(PageMsg::History(history::Msg::Loaded(Err(_))))
        ));
        // The wrong kind of reply is a failure too, never ignored.
        assert!(matches!(
            route(Reply::History, Ok(Response::Ok)),
            Message::Page(PageMsg::History(history::Msg::Loaded(Err(_))))
        ));
        assert!(matches!(
            route(Reply::Diagnostics, Err("x".into())),
            Message::Page(PageMsg::Diagnostics(diagnostics::Msg::Prepared(Err(_))))
        ));
        assert!(matches!(
            route(
                Reply::Settings(SettingField::Phone),
                Ok(Response::Doctor { checks: vec![] })
            ),
            Message::Page(PageMsg::Settings(settings::Msg::Saved(
                SettingField::Phone,
                Err(_)
            )))
        ));
        assert!(matches!(
            route(Reply::Doctor(DoctorFor::Leak, 4), Err("x".into())),
            Message::Page(PageMsg::Leak(leak::Msg::Checks(4, Err(_))))
        ));
        assert!(matches!(
            route(Reply::Doctor(DoctorFor::Settings, 2), Ok(Response::Ok)),
            Message::Page(PageMsg::Settings(settings::Msg::Checks(2, Err(_))))
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn a_service_that_never_answers_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("control.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        // Accepts and reads, never replies; keeps the connection open.
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
        });
        let started = tokio::time::Instant::now();
        let result = call_at(&path, Request::History { day: None }).await;
        // Other tests may switch the language meanwhile; either language's sentence will do.
        let text = result.unwrap_err();
        assert!(
            [cg_i18n::Lang::ZhCn, cg_i18n::Lang::En]
                .iter()
                .any(|&lang| text == cg_i18n::tr_in(lang, "gui.service_timeout", &[])),
            "{text}"
        );
        assert_eq!(started.elapsed(), TIMEOUT);
    }

    #[test]
    fn diagnostics_get_longer_to_answer() {
        let diagnostics = Request::Diagnostics {
            range: cg_ipc::DiagRange::Day,
            contact: None,
        };
        assert_eq!(timeout_for(&diagnostics), Duration::from_secs(90));
        assert_eq!(timeout_for(&Request::Doctor), Duration::from_secs(10));
    }

    #[test]
    fn replies_of_the_right_kind_reach_their_page() {
        assert!(matches!(
            route(Reply::Settings(SettingField::AutoConnect), Ok(Response::Ok)),
            Message::Page(PageMsg::Settings(settings::Msg::Saved(
                SettingField::AutoConnect,
                Ok(())
            )))
        ));
        assert!(matches!(
            route(
                Reply::Diagnostics,
                Ok(Response::Diagnostics { json: "{}".into() })
            ),
            Message::Page(PageMsg::Diagnostics(diagnostics::Msg::Prepared(Ok(json)))) if json == "{}"
        ));
        assert!(matches!(
            route(
                Reply::Doctor(DoctorFor::Leak, 1),
                Ok(Response::Doctor { checks: vec![] })
            ),
            Message::Page(PageMsg::Leak(leak::Msg::Checks(1, Ok(_))))
        ));
    }
}
