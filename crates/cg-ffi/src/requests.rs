//! `Command::Request` and the commands that are one request: a request to the service, its
//! reply turned into a view. Timeouts and every failure's text come from
//! `cg_ipc::client::call`.

use crate::api::{ReplyResult, ReplyValue};
use crate::views;
use cg_ipc::describe::ui;
use cg_ipc::{DiagRange, Endpoint, Request, Response, describe};
use jiff::Timestamp;
use jiff::tz::TimeZone;

pub(crate) async fn request(endpoint: &Endpoint, request: Request) -> ReplyResult {
    match cg_ipc::client::call(endpoint, request).await {
        Ok(response) => reply_to(response, Timestamp::now(), &TimeZone::system()),
        Err(message) => ReplyResult::err(message),
    }
}

/// `Command::DiagnosticsPrepare`: the bundle with the contact as typed, trimmed.
pub(crate) async fn diagnostics(
    endpoint: &Endpoint,
    range: DiagRange,
    email: &str,
    note: &str,
) -> ReplyResult {
    let contact = ui::contact(email, note);
    request(endpoint, Request::Diagnostics { range, contact }).await
}

fn reply_to(response: Response, now: Timestamp, tz: &TimeZone) -> ReplyResult {
    let ok = |value| ReplyResult::Ok { value };
    match response {
        Response::Ok | Response::Hello { .. } | Response::Status { .. } => ReplyResult::done(),
        Response::Doctor { checks } => ok(ReplyValue::Checks {
            rows: views::checks(&checks),
        }),
        Response::History { day } => ok(ReplyValue::History {
            view: views::history(&day, now, tz),
        }),
        Response::Diagnostics { json } => ok(ReplyValue::Diagnostics {
            json,
            file_name: cg_diag::save::file_name(now, tz),
        }),
        // `call` already turns these into text; kept so no reply is ever lost.
        Response::Error { reason } => ReplyResult::err(describe::reason_text(&reason)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cg_ipc::{Check, CheckId, Reason};

    fn now() -> Timestamp {
        "2026-10-30T08:09:10Z".parse().unwrap()
    }

    #[test]
    fn replies_are_done_or_the_reason() {
        let _g = cg_i18n::lock_for_tests();
        let reply = |r| reply_to(r, now(), &TimeZone::UTC);
        assert_eq!(reply(Response::Ok), ReplyResult::done());
        assert_eq!(
            reply(Response::Hello {
                protocol: cg_ipc::PROTOCOL_VERSION,
                version: "1.0.0".into(),
            }),
            ReplyResult::done()
        );
        let ReplyResult::Err { message } = reply(Response::Error {
            reason: Reason::BadRequest,
        }) else {
            panic!("expected Err");
        };
        assert_eq!(message, describe::reason_text(&Reason::BadRequest));
    }

    #[test]
    fn replies_with_content_are_views() {
        let _g = cg_i18n::lock_for_tests();
        let reply = |r| reply_to(r, now(), &TimeZone::UTC);
        let checks = vec![Check::new(CheckId::Tun, false)];
        assert_eq!(
            reply(Response::Doctor {
                checks: checks.clone()
            }),
            ReplyResult::Ok {
                value: ReplyValue::Checks {
                    rows: views::checks(&checks)
                }
            }
        );
        let day = crate::testing::middle_day();
        assert_eq!(
            reply(Response::History { day: day.clone() }),
            ReplyResult::Ok {
                value: ReplyValue::History {
                    view: views::history(&day, now(), &TimeZone::UTC)
                }
            }
        );
        assert_eq!(
            reply(Response::Diagnostics { json: "{}".into() }),
            ReplyResult::Ok {
                value: ReplyValue::Diagnostics {
                    json: "{}".into(),
                    file_name: "cellglide-diagnostics-20261030-080910.json".into()
                }
            }
        );
    }
}
