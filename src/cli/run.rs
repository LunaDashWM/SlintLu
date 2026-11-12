//! `slintlu run`: start the shell.

use slintlu::PlatformConfig;
use slintlu::compositors::Compositor;

use super::RunArgs;
use crate::config::{self, Watcher};
use crate::scripting::{ShellHost, globals, host};

pub fn run(args: RunArgs) -> Result<(), String> {
    let config = config::resolve_config(args.config.as_deref())?;
    let compositor = Compositor::detect();
    log::info!("compositor: {compositor}, renderer: {}", args.renderer);

    slintlu::init(PlatformConfig { renderer: args.renderer, backend: args.backend })
        .map_err(|e| e.to_string())?;
    if let Some(backend) = slintlu::active_backend() {
        log::info!("display server: {backend}");
    }

    let host = ShellHost::install(config.clone());
    globals::install(&host, &globals::ShellInfo { compositor });
    if !host.load() {
        return Err("the shell configuration has errors".into());
    }
    let _ipc = crate::scripting::ipc::Server::start(&slintlu::ipc::socket_path())
        .inspect_err(|e| log::warn!("IPC disabled: {e}"))
        .ok();
    slintlu::on_outputs_changed(|| {
        host::with(|host| host.outputs_changed());
    });

    let _watcher = if args.no_reload {
        None
    } else {
        let dir = config.parent().unwrap_or(&config).to_path_buf();
        Watcher::new(&[dir], || {
            host::with(|host| {
                log::info!("configuration changed, reloading");
                host.load();
            });
        })
        .inspect_err(|e| log::warn!("hot reload disabled: {e}"))
        .ok()
    };

    super::signals::install();
    let result = slint::run_event_loop_until_quit().map_err(|e| e.to_string());
    drop(host);
    host::shutdown();
    result
}
