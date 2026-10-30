//! In-process stand-ins for usbmuxd, the phone app and the tunnel backend.

pub mod battery_fixtures;
mod fake_backend;
mod fake_lockdown;
mod fake_mux;
mod fake_phone;
mod fake_probe;

pub use fake_backend::{FakeBackend, FinishCall, SharedFake};
pub use fake_lockdown::{
    DEVICE_CERT_PEM, DEVICE_KEY_PEM, FAKE_RELAY_PORT, FIXTURE_HOST_ID, FIXTURE_SYSTEM_BUID,
    FakeLockdown, HOST_CERT_PEM, HOST_KEY_PEM, fixture_pair_record, host_cert_der,
};
pub use fake_mux::FakeMux;
pub use fake_phone::{FakePhone, FakePhoneOptions, SESSION_A, SESSION_B, hello_json};
pub use fake_probe::{FakeProbe, FakeTunnelProbe, SharedProbe};

use cg_protocol::mux::Device;
use std::net::SocketAddr;
use tokio::net::TcpListener;

pub fn device(serial: &str, mux_id: u32) -> Device {
    Device {
        serial: serial.into(),
        mux_id,
    }
}

/// A TCP server that echoes every connection.
pub async fn echo_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = stream.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    addr
}

/// Where no usbmuxd answers: a missing socket, or on Windows a loopback port nobody listens on.
pub fn missing_mux() -> crate::mux::MuxEndpoint {
    #[cfg(unix)]
    {
        crate::mux::MuxEndpoint::Unix("/nonexistent/usbmuxd".into())
    }
    #[cfg(not(unix))]
    {
        let addr = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        crate::mux::MuxEndpoint::Tcp(addr)
    }
}
