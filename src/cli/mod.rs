//! Command line interface.

mod check;
mod info;
mod init;
mod msg;
mod outputs;
mod run;
mod signals;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use slintlu::{Backend, RendererKind};

/// Desktop shells written in the Slint language, for Wayland and X11.
#[derive(Parser, Debug)]
#[command(version, about, args_conflicts_with_subcommands = true)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    run: RunArgs,

    /// More logging (-v info, -vv debug, -vvv trace). RUST_LOG overrides it.
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run a shell (the default).
    Run(RunArgs),
    /// Compile a shell configuration and report problems without showing it.
    Check {
        /// Shell file or directory (defaults to the user configuration).
        path: Option<PathBuf>,
    },
    /// List the monitors the compositor reports.
    Outputs,
    /// Report what the compositor, graphics stack and desktop services support.
    Info,
    /// Create a starter configuration in ~/.config/slintlu.
    Init {
        /// Directory to create the configuration in.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Overwrite existing files.
        #[arg(long)]
        force: bool,
    },
    /// Print the directory holding the `@slintlu` library, for editor (slint-lsp) setup.
    LibraryPath,
    /// Send a command to the running shell.
    Msg {
        #[command(subcommand)]
        command: msg::MsgCommand,
    },
}

#[derive(Args, Debug, Clone)]
pub struct RunArgs {
    /// Shell file or directory containing shell.slint.
    #[arg(short, long, env = "SLINTLU_CONFIG")]
    config: Option<PathBuf>,

    /// Renderer: auto, software, opengl or vulkan.
    #[arg(short, long, env = "SLINTLU_RENDERER", default_value = "auto")]
    renderer: RendererKind,

    /// Display server: auto, wayland or x11.
    #[arg(short, long, env = "SLINTLU_BACKEND", default_value = "auto")]
    backend: Backend,

    /// Do not reload when configuration files change.
    #[arg(long)]
    no_reload: bool,
}

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    let result = match cli.command {
        None => run::run(cli.run),
        Some(Command::Run(args)) => run::run(args),
        Some(Command::Check { path }) => check::check(path),
        Some(Command::Outputs) => outputs::outputs(),
        Some(Command::Info) => info::info(),
        Some(Command::Init { dir, force }) => init::init(dir, force),
        Some(Command::Msg { command }) => msg::msg(command),
        Some(Command::LibraryPath) => crate::scripting::library::install()
            .map(|_| println!("{}", crate::scripting::library::dir().display()))
            .map_err(|e| e.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("slintlu: {message}");
            ExitCode::FAILURE
        }
    }
}

fn init_logging(verbose: u8) {
    let level = match verbose {
        0 => "warn,slintlu=info",
        1 => "info",
        2 => "debug,naga=info,wgpu_core=info,wgpu_hal=info",
        _ => "trace",
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level))
        .format_timestamp_millis()
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn bare_flags_mean_run() {
        let cli = Cli::try_parse_from(["slintlu", "-c", "x.slint", "-r", "vk"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.run.renderer, RendererKind::Vulkan);
        assert_eq!(cli.run.config.as_deref(), Some(std::path::Path::new("x.slint")));
    }

    #[test]
    fn msg_parses_values() {
        let cli =
            Cli::try_parse_from(["slintlu", "msg", "call", "Audio.set-volume", "0.5"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Msg { .. })));
    }

    #[test]
    fn subcommands_parse() {
        let cli = Cli::try_parse_from(["slintlu", "check", "shell.slint"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Check { path: Some(_) })));
    }
}
