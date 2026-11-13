use crate::Result;
use crate::mux::MuxStream;
use crate::wireless::ports::{BoxFuture, WirelessLink};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

/// A wireless link whose every port is a connection to one TCP server (an echo server by
/// default). Records the ports it was asked for.
pub struct FakeWirelessLink {
    phone_id: String,
    target: SocketAddr,
    opened: Mutex<Vec<u16>>,
}

impl FakeWirelessLink {
    pub async fn echo(phone_id: &str) -> Arc<FakeWirelessLink> {
        Arc::new(FakeWirelessLink {
            phone_id: phone_id.into(),
            target: super::echo_server().await,
            opened: Mutex::new(Vec::new()),
        })
    }

    /// The ports opened so far, in order.
    pub fn opened(&self) -> Vec<u16> {
        self.opened.lock().unwrap().clone()
    }
}

impl WirelessLink for FakeWirelessLink {
    fn phone_id(&self) -> &str {
        &self.phone_id
    }

    fn open(&self, port: u16) -> BoxFuture<'_, Result<MuxStream>> {
        self.opened.lock().unwrap().push(port);
        Box::pin(async move {
            let stream = tokio::net::TcpStream::connect(self.target).await?;
            Ok(Box::new(stream) as MuxStream)
        })
    }
}
