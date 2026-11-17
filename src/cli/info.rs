//! `slintlu info`: a compatibility report of the compositor, graphics and desktop services.

use slintlu::compositors::Compositor;
use slintlu::{PlatformConfig, RendererKind};
use zbus::fdo::DBusProxy;
use zbus::names::BusName;

use crate::config;

fn mark(ok: bool) -> &'static str {
    if ok { "✔" } else { "✘" }
}

/// `"noctalia (pid 1570)"` for the process owning a bus name, if any.
fn owner(connection: &zbus::Connection, name: &str) -> Option<String> {
    zbus::block_on(async {
        let dbus = DBusProxy::new(connection).await.ok()?;
        let bus = BusName::try_from(name).ok()?;
        let unique = dbus.get_name_owner(bus.clone()).await.ok()?;
        let pid = dbus.get_connection_unix_process_id(BusName::from(unique)).await.ok()?;
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
        Some(format!("{} (pid {pid})", comm.trim()))
    })
}

fn section(title: &str) {
    println!("\n{title}");
}

pub fn info() -> Result<(), String> {
    println!("SlintLu {}", env!("CARGO_PKG_VERSION"));
    println!("compositor: {}", Compositor::detect());
    match config::resolve_config(None) {
        Ok(path) => println!("config: {}", path.display()),
        Err(_) => println!("config: none (run `slintlu init`)"),
    }
    println!("IPC socket: {}", slintlu::ipc::socket_path().display());
    println!("icon theme: {}", slintlu::icons::default_theme_name());
    println!("language: {}", slintlu::i18n::current_language().unwrap_or_else(|| "default".into()));

    slintlu::init(PlatformConfig { renderer: RendererKind::Software, ..Default::default() })
        .map_err(|e| e.to_string())?;
    if let Some(backend) = slintlu::active_backend() {
        println!("display server: {backend}");
    }

    section("Outputs");
    for o in slintlu::outputs() {
        println!(
            "  {:<10} {}x{} scale {}  {}",
            o.name, o.width, o.height, o.scale_factor, o.description
        );
    }

    section("Wayland protocols");
    for (name, purpose, supported) in slintlu::debug::protocol_support() {
        println!("  {} {name:<42} {purpose}", mark(supported));
    }

    section("Renderers");
    for (kind, result) in slintlu::debug::probe_renderers() {
        match result {
            Ok(detail) => println!("  {} {:<9} {detail}", mark(true), kind.as_str()),
            Err(reason) => println!("  {} {:<9} {reason}", mark(false), kind.as_str()),
        }
    }

    section("Desktop services");
    println!("  {} audio (pactl)", mark(slintlu::audio::Pactl::detect().is_some()));
    println!(
        "  {} NetworkManager",
        mark(slintlu::dbus::network::NetworkMonitor::connect().is_ok())
    );
    println!(
        "  {} logind (power, lock signals, brightness)",
        mark(slintlu::dbus::power::Power::connect().is_ok())
    );
    let bluetooth =
        slintlu::dbus::bluetooth::Bluetooth::connect().and_then(|b| b.state()).map(|s| s.available);
    println!(
        "  {} BlueZ {}",
        mark(bluetooth == Ok(true)),
        if bluetooth == Ok(true) { "" } else { "(not running)" }
    );
    println!(
        "  {} compositor IPC (workspaces)",
        mark(slintlu::compositors::backend(Compositor::detect()).is_some())
    );
    println!("  {} screen backlight", mark(slintlu::system::backlight().is_some()));
    println!("  PAM service: {}", slintlu::security::default_service());

    if let Ok(session) = zbus::block_on(zbus::Connection::session()) {
        for (name, purpose) in [
            ("org.freedesktop.Notifications", "notification daemon"),
            ("org.kde.StatusNotifierWatcher", "tray watcher"),
            ("org.freedesktop.portal.Desktop", "desktop portal"),
        ] {
            match owner(&session, name) {
                Some(process) => println!("  {purpose}: {process}"),
                None => println!("  {purpose}: free (SlintLu will provide it)"),
            }
        }
    }
    Ok(())
}
