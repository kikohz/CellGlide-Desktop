//! Wire formats shared with the iPhone app and usbmuxd. No IO.

pub mod lockdown;
pub mod mux;
pub mod phone;
pub mod socks;

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("plist: {0}")]
    Plist(#[from] plist::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("malformed message: {0}")]
    Malformed(&'static str),
    #[error("message too large")]
    TooLarge,
    #[error("phone app incompatible")]
    Incompatible,
}

#[cfg(test)]
mod tests {
    #[test]
    fn error_text_is_for_logs() {
        assert_eq!(
            super::ProtocolError::Incompatible.to_string(),
            "phone app incompatible"
        );
    }
}
