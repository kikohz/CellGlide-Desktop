//! The one rustls client setup for user-process crates: ring as the provider, TLS 1.2
//! and 1.3, the bundled Mozilla roots, HTTP/1.1 only. The service has its own in `cg_core::tls`.

use rustls::{ClientConfig, RootCertStore};
use std::sync::{Arc, LazyLock};

/// `CellGlide-Linux/<version>` (`CellGlide-Windows/<version>` on Windows), sent as the
/// User-Agent of every request we make.
pub fn user_agent() -> String {
    #[cfg(windows)]
    let platform = "Windows";
    #[cfg(not(windows))]
    let platform = "Linux";
    format!("CellGlide-{platform}/{}", env!("CARGO_PKG_VERSION"))
}

static DEFAULT: LazyLock<Arc<ClientConfig>> = LazyLock::new(|| {
    let roots = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    client_config_with_roots(roots)
});

/// Trusts the bundled public roots.
pub fn client_config() -> Arc<ClientConfig> {
    DEFAULT.clone()
}

/// Same settings with other roots; tests use it to trust a local fake server.
pub fn client_config_with_roots(roots: RootCertStore) -> Arc<ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("ring supports TLS 1.2 and 1.3")
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Arc::new(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_names_the_platform_and_version() {
        let platform = if cfg!(windows) { "Windows" } else { "Linux" };
        assert_eq!(
            user_agent(),
            format!("CellGlide-{platform}/{}", env!("CARGO_PKG_VERSION"))
        );
        assert!(user_agent().starts_with(&format!("CellGlide-{platform}/1.")));
    }

    #[test]
    fn config_speaks_http1_and_is_shared() {
        let a = client_config();
        assert_eq!(a.alpn_protocols, vec![b"http/1.1".to_vec()]);
        assert!(Arc::ptr_eq(&a, &client_config()));
        let empty = client_config_with_roots(RootCertStore::empty());
        assert!(!Arc::ptr_eq(&a, &empty));
    }
}
