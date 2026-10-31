//! `Command::Request`: one request to the service. Timeouts and every failure's text come from
//! `cg_ipc::client::call`.

use crate::api::ReplyResult;
use cg_ipc::{Endpoint, Request, Response, describe};

pub(crate) async fn request(endpoint: &Endpoint, request: Request) -> ReplyResult {
    match cg_ipc::client::call(endpoint, request).await {
        Ok(response) => reply_to(response),
        Err(message) => ReplyResult::err(message),
    }
}

fn reply_to(response: Response) -> ReplyResult {
    match response {
        Response::Ok | Response::Hello { .. } | Response::Status { .. } => ReplyResult::done(),
        // Task 9 turns these into views.
        Response::Doctor { .. } | Response::History { .. } | Response::Diagnostics { .. } => {
            ReplyResult::done()
        }
        // `call` already turns these into text; kept so no reply is ever lost.
        Response::Error { reason } => ReplyResult::err(describe::reason_text(&reason)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cg_ipc::Reason;

    #[test]
    fn replies_are_done_or_the_reason() {
        assert_eq!(reply_to(Response::Ok), ReplyResult::done());
        assert_eq!(
            reply_to(Response::Hello {
                protocol: cg_ipc::PROTOCOL_VERSION,
                version: "1.0.0".into(),
            }),
            ReplyResult::done()
        );
        let ReplyResult::Err { message } = reply_to(Response::Error {
            reason: Reason::BadRequest,
        }) else {
            panic!("expected Err");
        };
        assert_eq!(message, describe::reason_text(&Reason::BadRequest));
    }
}
