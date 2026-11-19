//! Wi-Fi networks through NetworkManager: listing, scanning, connecting and disconnecting.
//!
//! Listing reads the access points of every Wi-Fi device on demand (a shell's network list
//! while it is shown), not on every signal-strength change the status monitor sees.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use zbus::{Connection, OwnedObjectPath, OwnedVariant, Variant, proxy};

/// NM_DEVICE_TYPE_WIFI.
const DEVICE_WIFI: u32 = 2;
/// NM_ACTIVE_CONNECTION_STATE_ACTIVATED and _DEACTIVATED.
const ACTIVATED: u32 = 2;
const DEACTIVATED: u32 = 4;
/// How long a connection may take before it counts as failed.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(40);

#[proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait Manager {
    fn get_devices(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
    fn activate_connection(
        &self,
        connection: &zbus::ObjectPath<'_>,
        device: &zbus::ObjectPath<'_>,
        specific_object: &zbus::ObjectPath<'_>,
    ) -> zbus::Result<OwnedObjectPath>;
    fn add_and_activate_connection(
        &self,
        connection: HashMap<&str, HashMap<&str, Variant<'_>>>,
        device: &zbus::ObjectPath<'_>,
        specific_object: &zbus::ObjectPath<'_>,
    ) -> zbus::Result<(OwnedObjectPath, OwnedObjectPath)>;
    fn deactivate_connection(&self, active: &zbus::ObjectPath<'_>) -> zbus::Result<()>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.Device",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Device {
    #[zbus(property)]
    fn device_type(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn active_connection(&self) -> zbus::Result<OwnedObjectPath>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.Device.Wireless",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Wireless {
    fn get_all_access_points(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
    fn request_scan(&self, options: HashMap<&str, Variant<'_>>) -> zbus::Result<()>;
    #[zbus(property)]
    fn active_access_point(&self) -> zbus::Result<OwnedObjectPath>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.AccessPoint",
    default_service = "org.freedesktop.NetworkManager"
)]
trait AccessPoint {
    #[zbus(property)]
    fn ssid(&self) -> zbus::Result<Vec<u8>>;
    #[zbus(property)]
    fn strength(&self) -> zbus::Result<u8>;
    #[zbus(property)]
    fn flags(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn wpa_flags(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn rsn_flags(&self) -> zbus::Result<u32>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.Settings",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager/Settings"
)]
trait Settings {
    fn list_connections(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.Settings.Connection",
    default_service = "org.freedesktop.NetworkManager"
)]
trait SavedConnection {
    fn get_settings(&self) -> zbus::Result<HashMap<String, HashMap<String, OwnedVariant>>>;
    fn delete(&self) -> zbus::Result<()>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.Connection.Active",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Active {
    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;
}

/// A network in range, strongest access point per name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WifiNetwork {
    pub ssid: String,
    /// 0..1
    pub strength: f32,
    /// Needs a password (WPA/WEP).
    pub secure: bool,
    /// The device is connected to it.
    pub active: bool,
    /// A saved connection exists: connecting needs no password.
    pub known: bool,
}

struct Radio {
    device: OwnedObjectPath,
    access_points: Vec<(OwnedObjectPath, WifiNetwork)>,
}

async fn system() -> zbus::Result<Connection> {
    Connection::system().await
}

async fn wifi_devices(connection: &Connection) -> zbus::Result<Vec<OwnedObjectPath>> {
    let mut devices = Vec::new();
    for path in ManagerProxy::new(connection).await?.get_devices().await? {
        let device = DeviceProxy::builder(connection).path(path.clone()).build().await?;
        if device.device_type().await.unwrap_or(0) == DEVICE_WIFI {
            devices.push(path);
        }
    }
    Ok(devices)
}

/// The SSIDs of saved Wi-Fi connections, with their paths.
async fn saved(connection: &Connection) -> zbus::Result<Vec<(String, OwnedObjectPath)>> {
    let mut result = Vec::new();
    for path in SettingsProxy::new(connection).await?.list_connections().await? {
        let saved = SavedConnectionProxy::builder(connection).path(path.clone()).build().await?;
        let Ok(settings) = saved.get_settings().await else { continue };
        let ssid = settings
            .get("802-11-wireless")
            .and_then(|wireless| wireless.get("ssid"))
            .and_then(|value| Vec::<u8>::try_from(value.clone()).ok());
        if let Some(ssid) = ssid {
            result.push((String::from_utf8_lossy(&ssid).into_owned(), path));
        }
    }
    Ok(result)
}

async fn radios(connection: &Connection) -> zbus::Result<Vec<Radio>> {
    let known = saved(connection).await.unwrap_or_default();
    let mut result = Vec::new();
    for device in wifi_devices(connection).await? {
        let wireless = WirelessProxy::builder(connection).path(device.clone()).build().await?;
        let active = wireless.active_access_point().await.ok();
        let mut access_points = Vec::new();
        for path in wireless.get_all_access_points().await.unwrap_or_default() {
            let ap = AccessPointProxy::builder(connection).path(path.clone()).build().await?;
            let ssid = String::from_utf8_lossy(&ap.ssid().await.unwrap_or_default()).into_owned();
            if ssid.is_empty() {
                continue;
            }
            let secure = ap.flags().await.unwrap_or(0) & 0x1 != 0
                || ap.wpa_flags().await.unwrap_or(0) != 0
                || ap.rsn_flags().await.unwrap_or(0) != 0;
            let network = WifiNetwork {
                known: known.iter().any(|(saved, _)| *saved == ssid),
                active: active.as_ref() == Some(&path),
                strength: f32::from(ap.strength().await.unwrap_or(0)) / 100.0,
                secure,
                ssid,
            };
            access_points.push((path, network));
        }
        result.push(Radio { device, access_points });
    }
    Ok(result)
}

/// The networks in range, the connected one first, then by signal.
pub fn networks() -> zbus::Result<Vec<WifiNetwork>> {
    let radios = zbus::block_on(async { radios(&system().await?).await })?;
    let mut list: Vec<WifiNetwork> = Vec::new();
    for (_, network) in radios.into_iter().flat_map(|r| r.access_points) {
        match list.iter_mut().find(|n| n.ssid == network.ssid) {
            Some(existing) => {
                existing.active |= network.active;
                if network.strength > existing.strength {
                    existing.strength = network.strength;
                }
            }
            None => list.push(network),
        }
    }
    list.sort_by(|a, b| {
        b.active.cmp(&a.active).then(b.strength.total_cmp(&a.strength)).then(a.ssid.cmp(&b.ssid))
    });
    Ok(list)
}

/// Asks every Wi-Fi device to look for networks (results arrive over a few seconds).
pub fn scan() -> zbus::Result<()> {
    zbus::block_on(async {
        let connection = system().await?;
        for device in wifi_devices(&connection).await? {
            let wireless = WirelessProxy::builder(&connection).path(device.clone()).build().await?;
            // NetworkManager refuses scans while one runs: that one serves too.
            let _ = wireless.request_scan(HashMap::new()).await;
        }
        Ok(())
    })
}

/// Connects to `ssid`: with its saved connection when there is one, else with a new one using
/// `password` (empty for an open network). Waits for the outcome; a new connection that fails
/// (a wrong password) is removed again, so the next try can give another password.
pub fn connect(ssid: &str, password: &str) -> Result<(), String> {
    let error = |e: zbus::Error| e.to_string();
    zbus::block_on(async {
        let connection = system().await.map_err(error)?;
        let radios = radios(&connection).await.map_err(error)?;
        let (device, access_point) = radios
            .iter()
            .flat_map(|r| r.access_points.iter().map(move |(path, n)| (r, path, n)))
            .filter(|(_, _, network)| network.ssid == ssid)
            .max_by(|a, b| a.2.strength.total_cmp(&b.2.strength))
            .map(|(radio, path, _)| (radio.device.clone(), path.clone()))
            .ok_or_else(|| format!("{ssid} is not in range."))?;
        let manager = ManagerProxy::new(&connection).await.map_err(error)?;
        let known = saved(&connection).await.map_err(error)?;
        let mut created = None;
        let active = match known.iter().find(|(saved, _)| saved == ssid) {
            Some((_, path)) => {
                manager.activate_connection(path, &device, &access_point).await.map_err(error)?
            }
            None => {
                let mut settings: HashMap<&str, HashMap<&str, Variant<'_>>> = HashMap::new();
                settings
                    .entry("802-11-wireless")
                    .or_default()
                    .insert("ssid", Variant::from(ssid.as_bytes().to_vec()));
                if !password.is_empty() {
                    let security = settings.entry("802-11-wireless-security").or_default();
                    security.insert("key-mgmt", Variant::from("wpa-psk"));
                    security.insert("psk", Variant::from(password));
                }
                let (path, active) = manager
                    .add_and_activate_connection(settings, &device, &access_point)
                    .await
                    .map_err(error)?;
                created = Some(path);
                active
            }
        };
        let state =
            ActiveProxy::builder(&connection).path(active.clone()).build().await.map_err(error)?;
        let started = Instant::now();
        loop {
            match state.state().await {
                Ok(ACTIVATED) => return Ok(()),
                // The active connection is gone once it fails.
                Ok(DEACTIVATED) | Err(_) => break,
                Ok(_) if started.elapsed() > CONNECT_TIMEOUT => break,
                Ok(_) => std::thread::sleep(Duration::from_millis(400)),
            }
        }
        if let Some(path) = created
            && let Ok(saved) = SavedConnectionProxy::builder(&connection).path(path).build().await
        {
            let _ = saved.delete().await;
        }
        Err(if password.is_empty() {
            format!("Could not connect to {ssid}.")
        } else {
            format!("Could not connect to {ssid}: check the password.")
        })
    })
}

/// Disconnects the Wi-Fi devices.
pub fn disconnect() -> zbus::Result<()> {
    zbus::block_on(async {
        let connection = system().await?;
        let manager = ManagerProxy::new(&connection).await?;
        for path in wifi_devices(&connection).await? {
            let device = DeviceProxy::builder(&connection).path(path.clone()).build().await?;
            if let Ok(active) = device.active_connection().await
                && active.as_str() != "/"
            {
                manager.deactivate_connection(&active).await?;
            }
        }
        Ok(())
    })
}
