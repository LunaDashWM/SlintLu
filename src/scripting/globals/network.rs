//! The `Network` global (NetworkManager).

use std::rc::Rc;

use slint_interpreter::Value;
use slintlu::dbus::network::{NetworkMonitor, NetworkState};
use slintlu::dbus::wifi::{self, WifiNetwork};

use super::worker::{run_action, spawn_watcher};
use crate::scripting::bridge::{bool_arg, list_value, string, string_arg, struct_value};
use crate::scripting::{ShellHost, host};

fn publish(host: &ShellHost, state: &NetworkState) {
    host.set_global("Network", "online", Value::Bool(state.online));
    host.set_global("Network", "connected", Value::Bool(state.connected));
    host.set_global("Network", "kind", string(state.kind.as_str()));
    host.set_global("Network", "name", string(&state.name));
    host.set_global("Network", "strength", Value::Number(state.strength.into()));
    host.set_global("Network", "wifi-enabled", Value::Bool(state.wifi_enabled));
}

fn network_value(network: &WifiNetwork) -> Value {
    struct_value([
        ("ssid", string(&network.ssid)),
        ("strength", Value::Number(network.strength.into())),
        ("secure", Value::Bool(network.secure)),
        ("active", Value::Bool(network.active)),
        ("known", Value::Bool(network.known)),
    ])
}

/// Lists the networks in range on a thread and publishes them.
fn list_networks() {
    std::thread::spawn(|| match wifi::networks() {
        Ok(networks) => host::post(move |host| {
            host.set_global("Network", "networks", list_value(networks.iter().map(network_value)));
            host.set_global("Network", "scanning", Value::Bool(false));
        }),
        Err(e) => {
            log::info!("network: cannot list Wi-Fi networks: {e}");
            host::post(|host| host.set_global("Network", "scanning", Value::Bool(false)));
        }
    });
}

pub fn install(host: &Rc<ShellHost>) {
    let monitor = match NetworkMonitor::connect() {
        Ok(monitor) => monitor,
        Err(e) => {
            log::info!("network: NetworkManager unavailable ({e})");
            host.set_global("Network", "available", Value::Bool(false));
            return;
        }
    };
    host.set_global("Network", "available", Value::Bool(true));

    let watcher = monitor.clone();
    spawn_watcher("network", move || {
        watcher
            .watch(|state| {
                let state = state.clone();
                host::post(move |host| publish(host, &state));
            })
            .map_err(std::io::Error::other)
    });

    host.on_callback(
        "Network",
        "scan",
        Rc::new(|_| {
            host::with(|host| host.set_global("Network", "scanning", Value::Bool(true)));
            // The list there is now, then again once the scan found more.
            list_networks();
            std::thread::spawn(|| {
                if let Err(e) = wifi::scan() {
                    log::info!("network: Wi-Fi scan: {e}");
                }
                std::thread::sleep(std::time::Duration::from_secs(3));
                list_networks();
            });
            Value::Void
        }),
    );
    host.on_callback(
        "Network",
        "connect-wifi",
        Rc::new(|args| {
            let ssid = string_arg(args, 0).unwrap_or_default();
            let password = string_arg(args, 1).unwrap_or_default();
            if ssid.is_empty() {
                return Value::Void;
            }
            host::with(|host| {
                host.set_global("Network", "connecting", string(&ssid));
                host.set_global("Network", "error", string(""));
            });
            std::thread::spawn(move || {
                let result = wifi::connect(&ssid, &password);
                host::post(move |host| {
                    host.set_global("Network", "connecting", string(""));
                    host.set_global("Network", "error", string(result.err().unwrap_or_default()));
                });
                list_networks();
            });
            Value::Void
        }),
    );
    host.on_callback(
        "Network",
        "disconnect-wifi",
        Rc::new(|_| {
            std::thread::spawn(|| {
                if let Err(e) = wifi::disconnect() {
                    log::warn!("network: disconnect: {e}");
                }
                list_networks();
            });
            Value::Void
        }),
    );
    host.on_callback(
        "Network",
        "set-wifi-enabled",
        Rc::new(move |args| {
            if let Some(enabled) = bool_arg(args, 0) {
                let monitor = monitor.clone();
                run_action("network", move || {
                    monitor.set_wifi_enabled(enabled).map_err(std::io::Error::other)
                });
            }
            Value::Void
        }),
    );
}
