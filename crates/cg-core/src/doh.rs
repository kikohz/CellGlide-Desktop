//! DNS-over-HTTPS (RFC 8484 POST) to the phone's control port. The phone speaks plain
//! HTTP/1.1 on loopback; usbmuxd carries it. Idle connections are kept for reuse.

use crate::phone::Phone;
use crate::resolver::{BoxFuture, Upstream};
use bytes::Bytes;
use cg_protocol::phone::port;
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1::{self, SendRequest};
use hyper::header::{ACCEPT, CONTENT_TYPE, HOST};
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use std::sync::Mutex;

const MAX_IDLE: usize = 4;
const DNS_MESSAGE: &str = "application/dns-message";

pub struct DohUpstream {
    phone: Phone,
    idle: Mutex<Vec<SendRequest<Full<Bytes>>>>,
}

impl DohUpstream {
    pub fn new(phone: Phone) -> Self {
        Self {
            phone,
            idle: Mutex::new(Vec::new()),
        }
    }

    fn take_idle(&self) -> Option<SendRequest<Full<Bytes>>> {
        let mut idle = self.idle.lock().unwrap();
        while let Some(sender) = idle.pop() {
            if !sender.is_closed() {
                return Some(sender);
            }
        }
        None
    }

    async fn connect(&self) -> Option<SendRequest<Full<Bytes>>> {
        let stream = self.phone.open(port::CONTROL).await.ok()?;
        let (sender, connection) = http1::handshake(TokioIo::new(stream)).await.ok()?;
        tokio::spawn(connection);
        Some(sender)
    }

    async fn post(&self, mut sender: SendRequest<Full<Bytes>>, query: Bytes) -> Option<Vec<u8>> {
        sender.ready().await.ok()?;
        let request = Request::builder()
            .method(Method::POST)
            .uri("/dns-query")
            .header(HOST, "localhost")
            .header(CONTENT_TYPE, DNS_MESSAGE)
            .header(ACCEPT, DNS_MESSAGE)
            .body(Full::new(query))
            .ok()?;
        let response = sender.send_request(request).await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        let body = response.into_body().collect().await.ok()?.to_bytes();
        let mut idle = self.idle.lock().unwrap();
        if idle.len() < MAX_IDLE {
            idle.push(sender);
        }
        Some(body.to_vec())
    }
}

impl Upstream for DohUpstream {
    fn exchange(&self, query: Vec<u8>) -> BoxFuture<'_, Option<Vec<u8>>> {
        Box::pin(async move {
            let query = Bytes::from(query);
            // The phone closes idle keep-alive connections; a POST that lands on one
            // fails, so it is retried once on a fresh connection.
            if let Some(sender) = self.take_idle()
                && let Some(answer) = self.post(sender, query.clone()).await
            {
                return Some(answer);
            }
            let sender = self.connect().await?;
            self.post(sender, query).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeMux, FakePhone, FakePhoneOptions, device};

    #[tokio::test]
    async fn posts_queries_over_one_kept_alive_connection() {
        let mux = FakeMux::start().await;
        mux.attach(device("aaaa", 1));
        let fake = FakePhone::start(&mux, FakePhoneOptions::default()).await;
        let doh = DohUpstream::new(Phone {
            endpoint: mux.endpoint(),
            device: device("aaaa", 1),
        });
        for id in 0..3u8 {
            let query = vec![0, id, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1];
            let answer = doh.exchange(query.clone()).await.unwrap();
            assert_eq!(answer[1], id);
            assert_eq!(answer[2] & 0x80, 0x80);
        }
        assert_eq!(fake.doh_queries(), 3);
    }

    #[tokio::test]
    async fn unreachable_phone_gives_none() {
        let mux = FakeMux::start().await;
        let doh = DohUpstream::new(Phone {
            endpoint: mux.endpoint(),
            device: device("gone", 9),
        });
        assert!(doh.exchange(vec![0; 17]).await.is_none());
    }
}
