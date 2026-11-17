//! The `Shell` global: environment information and process-level actions.

use std::process::{Command, Stdio};
use std::rc::Rc;

use slint_interpreter::Value;

use super::ShellInfo;
use crate::scripting::ShellHost;
use crate::scripting::bridge::{string, string_arg};
use crate::scripting::host;

pub fn install(host: &Rc<ShellHost>, info: &ShellInfo) {
    host.set_global("Shell", "compositor", string(info.compositor.as_str()));
    host.set_global("Shell", "version", string(env!("CARGO_PKG_VERSION")));
    host.set_global(
        "Shell",
        "resources",
        string(crate::scripting::library::resources_dir().to_string_lossy()),
    );

    host.on_callback(
        "Shell",
        "exec",
        Rc::new(|args| {
            if let Some(command) = string_arg(args, 0) {
                spawn_detached(&command);
            }
            Value::Void
        }),
    );
    host.on_callback(
        "Shell",
        "run",
        Rc::new(|args| {
            if let Some(command) = string_arg(args, 0) {
                run_detached(&command);
            }
            Value::Void
        }),
    );
    host.on_callback(
        "Shell",
        "env",
        Rc::new(|args| {
            string(std::env::var(string_arg(args, 0).unwrap_or_default()).unwrap_or_default())
        }),
    );
    host.on_callback(
        "Shell",
        "reload",
        Rc::new(|_| {
            // Never drop the windows from inside one of their own callbacks.
            host::post(|host| {
                host.load();
            });
            Value::Void
        }),
    );
    host.on_callback(
        "Shell",
        "quit",
        Rc::new(|_| {
            let _ = slint::quit_event_loop();
            Value::Void
        }),
    );
}

/// Runs `command` through `sh -c` in its own session, so it survives the shell.
pub fn spawn_detached(command: &str) {
    let mut command_line = Command::new("sh");
    command_line.arg("-c").arg(command);
    start(command_line, command);
}

/// Starts the program a user-written command line names, with its arguments, in its own
/// session and without a shell (see `split_command_line`): nothing in it is evaluated.
pub fn run_detached(line: &str) {
    let home = std::env::var("HOME").unwrap_or_default();
    match slintlu::launcher::split_command_line(line, &home) {
        Ok(words) => {
            let mut command = Command::new(&words[0]);
            command.args(&words[1..]);
            start(command, line);
        }
        Err(e) => log::warn!("cannot run `{line}`: {e}"),
    }
}

fn start(mut command: Command, line: &str) {
    command.stdin(Stdio::null()).stdout(Stdio::null());
    match slintlu::launcher::detach(&mut command).spawn() {
        // Reap the child in the background so it never becomes a zombie.
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(e) => log::error!("cannot run `{line}`: {e}"),
    }
}
