//! rustls setup shared inside cg-core: ring as the provider, TLS 1.2 and 1.3.

use rustls::crypto::CryptoProvider;
use rustls::{ClientConfig, ConfigBuilder, WantsVerifier};
use std::sync::Arc;

pub(crate) fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// A client config builder waiting for its certificate verifier.
pub(crate) fn client_builder(
    provider: Arc<CryptoProvider>,
) -> ConfigBuilder<ClientConfig, WantsVerifier> {
    ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("ring supports TLS 1.2 and 1.3")
}
