//! The temporary access point the phone joins (design §6): a Wi-Fi Direct autonomous group
//! owner with a legacy (WPA2) access point, network name and password ours. Windows creates the
//! "Microsoft Wi-Fi Direct Virtual Adapter", gives it 192.168.137.1/24 and hands out addresses;
//! nothing here writes any setting on that adapter, on the Wi-Fi the computer is connected to,
//! or on the system's Mobile Hotspot (which is only ever read). The access point exists while
//! an [`AccessPoint`] does: `stop`/drop stops the publisher, and it also vanishes with the
//! process.
//!
//! Call order as proven on 2026-11-13: publisher with `IsAutonomousGroupOwnerEnabled`,
//! `ListenStateDiscoverability = Normal` (with `None` the iPhone did not see the network) and
//! `LegacySettings`; a `WiFiDirectConnectionListener` registered before `Start`, whose requests
//! are accepted with `WiFiDirectDevice::FromIdAsync`, keeping the device object (dropping it
//! disconnects the client).

use std::fmt;
use std::net::IpAddr;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::Devices::WiFiDirect::{
    WiFiDirectAdvertisementListenStateDiscoverability, WiFiDirectAdvertisementPublisher,
    WiFiDirectAdvertisementPublisherStatus, WiFiDirectAdvertisementPublisherStatusChangedEventArgs,
    WiFiDirectConnectionListener, WiFiDirectConnectionRequestedEventArgs,
    WiFiDirectConnectionStatus, WiFiDirectDevice, WiFiDirectError,
};
use windows::Foundation::TypedEventHandler;
use windows::Networking::Connectivity::NetworkInformation;
use windows::Networking::NetworkOperators::{
    NetworkOperatorTetheringManager, TetheringOperationalState,
};
use windows::Security::Credentials::PasswordCredential;
use windows::Win32::Foundation::{HANDLE, NO_ERROR};
use windows::Win32::NetworkManagement::WiFi::{
    WLAN_INTERFACE_INFO_LIST, WlanCloseHandle, WlanEnumInterfaces, WlanFreeMemory, WlanOpenHandle,
};
use windows::core::{HSTRING, Ref};

/// How long `start` waits for the publisher to report `Started` or `Aborted`.
const START_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApError {
    /// The computer has no wireless adapter.
    NoAdapter,
    /// Wireless is switched off (airplane mode or the Wi-Fi switch).
    RadioOff,
    /// The adapter is busy with something else; `mobile_hotspot` says whether that is the
    /// system's Mobile Hotspot (the user can turn it off), as read just now.
    InUse { mobile_hotspot: bool },
    /// The adapter or driver cannot do this, or the publisher never answered ("timeout").
    Unsupported(String),
}

impl fmt::Display for ApError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApError::NoAdapter => f.write_str("no wireless adapter"),
            ApError::RadioOff => f.write_str("wireless is switched off"),
            ApError::InUse {
                mobile_hotspot: true,
            } => f.write_str("Mobile Hotspot is on"),
            ApError::InUse {
                mobile_hotspot: false,
            } => f.write_str("the wireless adapter is in use by another feature"),
            ApError::Unsupported(detail) => write!(f, "not supported: {detail}"),
        }
    }
}

impl std::error::Error for ApError {}

/// A device that joined the access point: kept so the connection stays up.
struct Client {
    device: WiFiDirectDevice,
    local: IpAddr,
    remote: IpAddr,
}

pub struct AccessPoint {
    publisher: WiFiDirectAdvertisementPublisher,
    status_token: i64,
    listener: WiFiDirectConnectionListener,
    request_token: i64,
    clients: Arc<Mutex<Vec<Client>>>,
}

impl AccessPoint {
    /// Starts the access point with this network name and WPA2 passphrase; returns once the
    /// publisher reports `Started`, or the classified failure.
    pub fn start(ssid: &str, passphrase: &str) -> Result<AccessPoint, ApError> {
        let winrt = |e: windows::core::Error| ApError::Unsupported(format!("{e}"));
        let publisher = WiFiDirectAdvertisementPublisher::new().map_err(winrt)?;
        let advertisement = publisher.Advertisement().map_err(winrt)?;
        advertisement
            .SetIsAutonomousGroupOwnerEnabled(true)
            .map_err(winrt)?;
        advertisement
            .SetListenStateDiscoverability(
                WiFiDirectAdvertisementListenStateDiscoverability::Normal,
            )
            .map_err(winrt)?;
        let legacy = advertisement.LegacySettings().map_err(winrt)?;
        legacy.SetIsEnabled(true).map_err(winrt)?;
        legacy.SetSsid(&HSTRING::from(ssid)).map_err(winrt)?;
        let credential = PasswordCredential::new().map_err(winrt)?;
        credential
            .SetPassword(&HSTRING::from(passphrase))
            .map_err(winrt)?;
        legacy.SetPassphrase(&credential).map_err(winrt)?;

        let (status_sender, statuses) = mpsc::channel();
        let status_token = publisher
            .StatusChanged(&TypedEventHandler::new(
                move |_: Ref<WiFiDirectAdvertisementPublisher>,
                      args: Ref<WiFiDirectAdvertisementPublisherStatusChangedEventArgs>| {
                    if let Some(args) = args.as_ref() {
                        let status = args.Status()?;
                        let error = args.Error()?;
                        tracing::info!(
                            "access point: publisher status {} error {}",
                            status.0,
                            error.0
                        );
                        let _ = status_sender.send((status, error));
                    }
                    Ok(())
                },
            ))
            .map_err(winrt)?;

        let clients: Arc<Mutex<Vec<Client>>> = Arc::default();
        let listener = WiFiDirectConnectionListener::new().map_err(winrt)?;
        let accepted = clients.clone();
        let request_token = listener
            .ConnectionRequested(&TypedEventHandler::new(
                move |_: Ref<WiFiDirectConnectionListener>,
                      args: Ref<WiFiDirectConnectionRequestedEventArgs>| {
                    if let Some(args) = args.as_ref() {
                        let request = args.GetConnectionRequest()?;
                        let clients = accepted.clone();
                        // The handler must not block: accepting waits on an async operation.
                        std::thread::spawn(move || {
                            if let Err(error) = accept(&request, &clients) {
                                tracing::warn!("access point: accepting a client failed: {error}");
                            }
                            let _ = request.Close();
                        });
                    }
                    Ok(())
                },
            ))
            .map_err(winrt)?;

        let access_point = AccessPoint {
            publisher,
            status_token,
            listener,
            request_token,
            clients,
        };
        if let Err(error) = access_point.publisher.Start() {
            drop(access_point);
            return Err(match wlan_interface_count() {
                Some(0) => ApError::NoAdapter,
                _ => winrt(error),
            });
        }
        let deadline = std::time::Instant::now() + START_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match statuses.recv_timeout(left) {
                Ok((WiFiDirectAdvertisementPublisherStatus::Started, _)) => {
                    return Ok(access_point);
                }
                Ok((WiFiDirectAdvertisementPublisherStatus::Aborted, error)) => {
                    drop(access_point);
                    return Err(classify(wlan_interface_count(), error, mobile_hotspot_on));
                }
                Ok(_) => continue,
                Err(_) => {
                    drop(access_point);
                    return Err(ApError::Unsupported("timeout".into()));
                }
            }
        }
    }

    /// The devices connected right now: (the access point's address, the device's address).
    pub fn clients(&self) -> Vec<(IpAddr, IpAddr)> {
        let Ok(mut clients) = self.clients.lock() else {
            return Vec::new();
        };
        clients.retain(|client| {
            let connected = client.device.ConnectionStatus().ok()
                == Some(WiFiDirectConnectionStatus::Connected);
            if !connected {
                let _ = client.device.Close();
            }
            connected
        });
        clients
            .iter()
            .map(|client| (client.local, client.remote))
            .collect()
    }

    /// Stops the access point (as dropping does).
    pub fn stop(self) {}
}

impl Drop for AccessPoint {
    fn drop(&mut self) {
        let _ = self.listener.RemoveConnectionRequested(self.request_token);
        let _ = self.publisher.RemoveStatusChanged(self.status_token);
        if let Err(error) = self.publisher.Stop() {
            tracing::warn!("access point: stopping the publisher failed: {error}");
        }
        if let Ok(mut clients) = self.clients.lock() {
            for client in clients.drain(..) {
                let _ = client.device.Close();
            }
        }
        tracing::info!("access point: stopped");
    }
}

/// Accepts one connection request: opening the device is what admits it to the group.
fn accept(
    request: &windows::Devices::WiFiDirect::WiFiDirectConnectionRequest,
    clients: &Mutex<Vec<Client>>,
) -> windows::core::Result<()> {
    let information = request.DeviceInformation()?;
    let id = information.Id()?;
    tracing::info!(
        "access point: connection requested by {} ({id})",
        information.Name()?
    );
    let device = WiFiDirectDevice::FromIdAsync(&id)?.join()?;
    let pairs = device.GetConnectionEndpointPairs()?;
    let mut endpoints = None;
    for pair in pairs {
        let local = host_address(pair.LocalHostName().ok());
        let remote = host_address(pair.RemoteHostName().ok());
        tracing::info!("access point: client connected: local {local:?} remote {remote:?}");
        if let (Some(local), Some(remote)) = (local, remote) {
            endpoints.get_or_insert((local, remote));
        }
    }
    let Some((local, remote)) = endpoints else {
        tracing::warn!("access point: the client has no IP endpoint");
        let _ = device.Close();
        return Ok(());
    };
    if let Ok(mut clients) = clients.lock() {
        clients.push(Client {
            device,
            local,
            remote,
        });
    }
    Ok(())
}

fn host_address(host: Option<windows::Networking::HostName>) -> Option<IpAddr> {
    let name = host?.CanonicalName().ok()?.to_string();
    // An IPv6 link-local name may carry a zone ("fe80::1%12").
    name.split('%').next()?.parse().ok()
}

/// Design §6.1: why the publisher aborted. `wlan_interfaces` is the number of wireless
/// adapters (None when it could not be read); `mobile_hotspot` is asked only for
/// `ResourceInUse`.
fn classify(
    wlan_interfaces: Option<usize>,
    error: WiFiDirectError,
    mobile_hotspot: impl FnOnce() -> bool,
) -> ApError {
    if wlan_interfaces == Some(0) {
        ApError::NoAdapter
    } else if error == WiFiDirectError::RadioNotAvailable {
        ApError::RadioOff
    } else if error == WiFiDirectError::ResourceInUse {
        ApError::InUse {
            mobile_hotspot: mobile_hotspot(),
        }
    } else {
        ApError::Unsupported(format!(
            "the publisher aborted (WiFiDirectError {})",
            error.0
        ))
    }
}

/// The number of wireless adapters, read with `WlanEnumInterfaces`; None if the WLAN service
/// cannot be asked.
fn wlan_interface_count() -> Option<usize> {
    let mut version = 0u32;
    let mut handle = HANDLE::default();
    // SAFETY: both out-pointers are valid; client version 2 is Vista and later.
    if unsafe { WlanOpenHandle(2, None, &mut version, &mut handle) } != NO_ERROR.0 {
        return None;
    }
    let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
    // SAFETY: an open handle; on success the list is ours to free with WlanFreeMemory.
    let count = if unsafe { WlanEnumInterfaces(handle, None, &mut list) } == NO_ERROR.0
        && !list.is_null()
    {
        // SAFETY: the list WlanEnumInterfaces returned.
        let count = unsafe { (*list).dwNumberOfItems } as usize;
        // SAFETY: as above, freed once.
        unsafe { WlanFreeMemory(list.cast()) };
        Some(count)
    } else {
        None
    };
    // SAFETY: the handle opened above, closed once.
    unsafe { WlanCloseHandle(handle, None) };
    count
}

/// Whether the system's Mobile Hotspot is on, read only. Without an Internet connection
/// profile there is no tethering manager to ask, which counts as "not on".
pub fn mobile_hotspot_on() -> bool {
    let Ok(profile) = NetworkInformation::GetInternetConnectionProfile() else {
        return false;
    };
    let Ok(manager) = NetworkOperatorTetheringManager::CreateFromConnectionProfile(&profile) else {
        return false;
    };
    manager.TetheringOperationalState().ok() == Some(TetheringOperationalState::On)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn not_asked() -> bool {
        panic!("the hotspot is only read for ResourceInUse")
    }

    #[test]
    fn aborts_are_classified_as_in_the_design() {
        assert_eq!(
            classify(Some(0), WiFiDirectError::RadioNotAvailable, not_asked),
            ApError::NoAdapter
        );
        assert_eq!(
            classify(Some(1), WiFiDirectError::RadioNotAvailable, not_asked),
            ApError::RadioOff
        );
        assert_eq!(
            classify(None, WiFiDirectError::RadioNotAvailable, not_asked),
            ApError::RadioOff
        );
        assert_eq!(
            classify(Some(1), WiFiDirectError::ResourceInUse, || true),
            ApError::InUse {
                mobile_hotspot: true
            }
        );
        assert_eq!(
            classify(Some(1), WiFiDirectError::ResourceInUse, || false),
            ApError::InUse {
                mobile_hotspot: false
            }
        );
        assert!(matches!(
            classify(Some(1), WiFiDirectError::Success, not_asked),
            ApError::Unsupported(_)
        ));
    }

    #[test]
    fn host_names_become_addresses() {
        let name =
            |text: &str| windows::Networking::HostName::CreateHostName(&HSTRING::from(text)).ok();
        assert_eq!(
            host_address(name("192.168.137.205")),
            Some("192.168.137.205".parse().unwrap())
        );
        assert_eq!(
            host_address(name("fe80::1%12")),
            Some("fe80::1".parse().unwrap())
        );
        assert_eq!(host_address(name("phone.local")), None);
        assert_eq!(host_address(None), None);
    }

    #[test]
    fn the_access_point_can_move_between_threads() {
        fn send<T: Send>() {}
        send::<AccessPoint>();
    }
}
