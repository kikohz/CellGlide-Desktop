//! The wireless commands (`Command::Wireless*`): each is one request to the service. Removing or
//! pairing an iPhone again and a new network name change what the settings page lists, so after
//! those the subscription asks the service for the paired iPhones again at once.

use crate::api::{Command, ReplyResult};
use crate::runtime::Context;
use cg_ipc::Request;

/// The service request a wireless command stands for, and whether the paired iPhones may have
/// changed once it went through; `None` for any other command.
pub(crate) fn request_for(command: &Command) -> Option<(Request, bool)> {
    Some(match command {
        Command::WirelessStart => (Request::ConnectWireless, false),
        Command::WirelessCancel => (Request::CancelWireless, false),
        Command::WirelessConfirm { matches } => (Request::ConfirmCode { matches: *matches }, false),
        Command::WirelessForget { peer_id } => (
            Request::ForgetPhone {
                peer_id: peer_id.clone(),
            },
            true,
        ),
        Command::WirelessRepair { peer_id } => (
            Request::RepairPhone {
                peer_id: peer_id.clone(),
            },
            true,
        ),
        Command::WirelessRegenerate => (Request::RegenerateAccessPoint, true),
        Command::WirelessInfo => (Request::WirelessInfo, false),
        _ => return None,
    })
}

/// Runs a wireless command (see [`request_for`]).
pub(crate) async fn run(command: Command, context: &Context) -> ReplyResult {
    let Some((request, changes_list)) = request_for(&command) else {
        unreachable!("only wireless commands come here")
    };
    let result = crate::requests::request(&context.config.endpoint, request).await;
    if changes_list && matches!(result, ReplyResult::Ok { .. }) {
        context.shared.wireless_changed();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_command_is_its_request() {
        let peer = || "A1".to_string();
        let cases = [
            (Command::WirelessStart, Request::ConnectWireless, false),
            (Command::WirelessCancel, Request::CancelWireless, false),
            (
                Command::WirelessConfirm { matches: true },
                Request::ConfirmCode { matches: true },
                false,
            ),
            (
                Command::WirelessForget { peer_id: peer() },
                Request::ForgetPhone { peer_id: peer() },
                true,
            ),
            (
                Command::WirelessRepair { peer_id: peer() },
                Request::RepairPhone { peer_id: peer() },
                true,
            ),
            (
                Command::WirelessRegenerate,
                Request::RegenerateAccessPoint,
                true,
            ),
            (Command::WirelessInfo, Request::WirelessInfo, false),
        ];
        for (command, request, changes) in cases {
            assert_eq!(
                request_for(&command),
                Some((request, changes)),
                "{command:?}"
            );
        }
        assert_eq!(request_for(&Command::Toggle), None);
    }
}
