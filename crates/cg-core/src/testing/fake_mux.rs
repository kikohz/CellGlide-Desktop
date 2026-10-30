use crate::mux::{MuxEndpoint, read_message};
use cg_protocol::mux::{Device, MuxEvent, encode_message};
use plist::{Dictionary, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

#[derive(Default)]
struct State {
    devices: Vec<Device>,
    routes: HashMap<u16, SocketAddr>,
    /// PairRecordData by serial.
    pair_records: HashMap<String, Vec<u8>>,
    listeners: Vec<mpsc::UnboundedSender<MuxEvent>>,
}

/// A usbmuxd on a temporary Unix socket; on Windows on TCP 127.0.0.1, as Apple Mobile
/// Device Service. `Connect` to a routed port is piped to a local TCP server.
pub struct FakeMux {
    #[cfg(unix)]
    _dir: tempfile::TempDir,
    endpoint: MuxEndpoint,
    state: Arc<Mutex<State>>,
    task: JoinHandle<()>,
}

impl FakeMux {
    pub async fn start() -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        #[cfg(unix)]
        let (dir, endpoint, task) = {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("mux.sock");
            let listener = tokio::net::UnixListener::bind(&path).unwrap();
            let task = tokio::spawn(async move {
                while let Ok((conn, _)) = listener.accept().await {
                    tokio::spawn(serve(conn, shared.clone()));
                }
            });
            (dir, MuxEndpoint::Unix(path), task)
        };
        #[cfg(not(unix))]
        let (endpoint, task) = {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let task = tokio::spawn(async move {
                while let Ok((conn, _)) = listener.accept().await {
                    tokio::spawn(serve(conn, shared.clone()));
                }
            });
            (MuxEndpoint::Tcp(addr), task)
        };
        Self {
            #[cfg(unix)]
            _dir: dir,
            endpoint,
            state,
            task,
        }
    }

    pub fn endpoint(&self) -> MuxEndpoint {
        self.endpoint.clone()
    }

    pub fn attach(&self, device: Device) {
        let mut s = self.state.lock().unwrap();
        s.devices.push(device.clone());
        s.listeners
            .retain(|l| l.send(MuxEvent::Attached(device.clone())).is_ok());
    }

    pub fn detach(&self, mux_id: u32) {
        let mut s = self.state.lock().unwrap();
        s.devices.retain(|d| d.mux_id != mux_id);
        s.listeners
            .retain(|l| l.send(MuxEvent::Detached(mux_id)).is_ok());
    }

    pub fn route(&self, port: u16, to: SocketAddr) {
        self.state.lock().unwrap().routes.insert(port, to);
    }

    /// A pairing record whose contents nobody reads.
    pub fn add_pair_record(&self, serial: &str) {
        self.state
            .lock()
            .unwrap()
            .pair_records
            .insert(serial.into(), vec![1]);
    }

    /// A pairing record with real contents, stored as usbmuxd does: an XML plist in PairRecordData.
    pub fn add_pair_record_with(&self, serial: &str, record: Dictionary) {
        let mut data = Vec::new();
        plist::to_writer_xml(&mut data, &Value::Dictionary(record)).unwrap();
        self.state
            .lock()
            .unwrap()
            .pair_records
            .insert(serial.into(), data);
    }

    /// Listen connections whose client is still connected.
    pub fn listener_count(&self) -> usize {
        let s = self.state.lock().unwrap();
        s.listeners.iter().filter(|l| !l.is_closed()).count()
    }

    /// Closes every Listen connection, as when usbmuxd restarts.
    pub fn drop_listeners(&self) {
        self.state.lock().unwrap().listeners.clear();
    }
}

impl Drop for FakeMux {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn result(number: u64) -> Dictionary {
    let mut d = Dictionary::new();
    d.insert("MessageType".into(), "Result".into());
    d.insert("Number".into(), Value::Integer(number.into()));
    d
}

fn device_dict(device: &Device) -> Dictionary {
    let mut props = Dictionary::new();
    props.insert("ConnectionType".into(), "USB".into());
    props.insert("SerialNumber".into(), device.serial.as_str().into());
    let mut d = Dictionary::new();
    d.insert(
        "DeviceID".into(),
        Value::Integer(u64::from(device.mux_id).into()),
    );
    d.insert("Properties".into(), Value::Dictionary(props));
    d
}

async fn send<S: AsyncWrite + Unpin>(conn: &mut S, dict: &Dictionary) -> std::io::Result<()> {
    conn.write_all(&encode_message(dict, 1).unwrap()).await
}

async fn serve<S: AsyncRead + AsyncWrite + Unpin>(mut conn: S, state: Arc<Mutex<State>>) {
    let Ok(request) = read_message(&mut conn).await else {
        return;
    };
    let uint = |key: &str| request.get(key).and_then(Value::as_unsigned_integer);
    match request.get("MessageType").and_then(Value::as_string) {
        Some("ListDevices") => {
            let list = state
                .lock()
                .unwrap()
                .devices
                .iter()
                .map(|d| Value::Dictionary(device_dict(d)))
                .collect();
            let mut reply = Dictionary::new();
            reply.insert("DeviceList".into(), Value::Array(list));
            let _ = send(&mut conn, &reply).await;
        }
        Some("Listen") => {
            let (tx, mut rx) = mpsc::unbounded_channel();
            state.lock().unwrap().listeners.push(tx);
            if send(&mut conn, &result(0)).await.is_err() {
                return;
            }
            let mut probe = [0u8; 1];
            loop {
                // Like usbmuxd, notice a client that hangs up while no events flow.
                let event = tokio::select! {
                    event = rx.recv() => event,
                    read = conn.read(&mut probe) => match read {
                        Ok(0) | Err(_) => return,
                        Ok(_) => continue,
                    },
                };
                let Some(event) = event else { return };
                let message = match event {
                    MuxEvent::Attached(d) => {
                        let mut m = device_dict(&d);
                        m.insert("MessageType".into(), "Attached".into());
                        m
                    }
                    MuxEvent::Detached(id) => {
                        let mut m = Dictionary::new();
                        m.insert("MessageType".into(), "Detached".into());
                        m.insert("DeviceID".into(), Value::Integer(u64::from(id).into()));
                        m
                    }
                    MuxEvent::Other => continue,
                };
                if send(&mut conn, &message).await.is_err() {
                    return;
                }
            }
        }
        Some("Connect") => {
            let id = uint("DeviceID").unwrap_or(u64::MAX);
            let wire = uint("PortNumber").unwrap_or(0) as u16;
            let port = u16::from_be_bytes(wire.to_le_bytes());
            let target = {
                let s = state.lock().unwrap();
                s.devices
                    .iter()
                    .any(|d| u64::from(d.mux_id) == id)
                    .then(|| s.routes.get(&port).copied())
                    .flatten()
            };
            let upstream = match target {
                Some(addr) => TcpStream::connect(addr).await.ok(),
                None => None,
            };
            let Some(mut upstream) = upstream else {
                let _ = send(&mut conn, &result(3)).await;
                return;
            };
            if send(&mut conn, &result(0)).await.is_ok() {
                let _ = tokio::io::copy_bidirectional(&mut conn, &mut upstream).await;
            }
        }
        Some("ReadPairRecord") => {
            let serial = request
                .get("PairRecordID")
                .and_then(Value::as_string)
                .unwrap_or_default();
            let record = state.lock().unwrap().pair_records.get(serial).cloned();
            let reply = match record {
                Some(data) => {
                    let mut d = Dictionary::new();
                    d.insert("PairRecordData".into(), Value::Data(data));
                    d
                }
                None => result(2),
            };
            let _ = send(&mut conn, &reply).await;
        }
        _ => {
            let _ = send(&mut conn, &result(1)).await;
        }
    }
}
