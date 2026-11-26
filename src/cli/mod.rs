//! Command line interface.

mod check;
mod info;
mod init;
mod msg;
mod outputs;
mod render;
mod run;
mod signals;
mod workspaces;

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
    /// List the workspaces and windows the compositor reports.
    Workspaces,
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
    /// Play a component on a clock of its own and write each frame as a PNG.
    Render {
        /// The .slint file.
        path: PathBuf,
        /// The component to render (default: the last one the file exports).
        #[arg(long)]
        component: Option<String>,
        /// The directory the frames go to.
        #[arg(long)]
        out: PathBuf,
        /// What frame file names start with; four digits and .png follow.
        #[arg(long, default_value = "frame-")]
        prefix: String,
        /// Frame size, WIDTHxHEIGHT.
        #[arg(long, default_value = "300x300", value_parser = parse_size)]
        size: (u32, u32),
        #[arg(long, default_value_t = 30.0)]
        fps: f64,
        /// How much of the animation to render.
        #[arg(long, default_value_t = 2.0)]
        seconds: f64,
    },
}

fn parse_size(text: &str) -> Result<(u32, u32), String> {
    let (width, height) = text.split_once('x').ok_or("expected WIDTHxHEIGHT")?;
    let parse = |n: &str| n.parse::<u32>().ok().filter(|n| *n > 0).ok_or("expected WIDTHxHEIGHT");
    Ok((parse(width)?, parse(height)?))
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
        Some(Command::Workspaces) => workspaces::workspaces(),
        Some(Command::Info) => info::info(),
        Some(Command::Init { dir, force }) => init::init(dir, force),
        Some(Command::Msg { command }) => msg::msg(command),
        Some(Command::Render { path, component, out, prefix, size, fps, seconds }) => {
            render::render(render::RenderOptions {
                path,
                component,
                out,
                prefix,
                width: size.0,
                height: size.1,
                fps,
                seconds,
            })
        }
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
