//! The desktop entry `Exec` key: unquoting, field codes and starting the process.

use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use super::entry::AppEntry;

#[derive(Debug)]
pub enum LaunchError {
    EmptyCommand,
    Spawn(std::io::Error),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyCommand => f.write_str("the application has an empty Exec line"),
            Self::Spawn(e) => write!(f, "cannot start the application: {e}"),
        }
    }
}

impl std::error::Error for LaunchError {}

/// Splits an `Exec` value into arguments and expands field codes (no files or URLs are passed,
/// so `%f %F %u %U` disappear).
pub fn parse_exec(exec: &str, app: &AppEntry) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut has_arg = false;
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                has_arg = true;
            }
            '\\' if in_quotes => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            c if c.is_whitespace() && !in_quotes => {
                if has_arg {
                    args.push(std::mem::take(&mut current));
                    has_arg = false;
                }
            }
            '%' => {
                has_arg = true;
                match chars.next() {
                    Some('%') => current.push('%'),
                    Some('c') => current.push_str(&app.name),
                    Some('k') => current.push_str(&app.path.to_string_lossy()),
                    Some('i') if !app.icon.is_empty() => {
                        args.push("--icon".into());
                        current.push_str(&app.icon);
                    }
                    // %f %F %u %U %d %D %n %N %v %m and unknown codes expand to nothing.
                    _ => {}
                }
            }
            c => {
                current.push(c);
                has_arg = true;
            }
        }
    }
    if has_arg {
        args.push(current);
    }
    args.retain(|a| !a.is_empty());
    args
}

/// A terminal emulator for `Terminal=true` applications, as `[program, flags…]` that precede
/// the command.
fn terminal() -> Vec<String> {
    if let Ok(term) = std::env::var("TERMINAL")
        && !term.is_empty()
    {
        return vec![term, "-e".into()];
    }
    let in_path = |name: &str| {
        std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
    };
    let known: [(&str, &[&str]); 9] = [
        ("xdg-terminal-exec", &[]),
        ("foot", &[]),
        ("kitty", &[]),
        ("alacritty", &["-e"]),
        ("wezterm", &["start", "--"]),
        ("ghostty", &["-e"]),
        ("konsole", &["-e"]),
        ("gnome-terminal", &["--"]),
        ("xterm", &["-e"]),
    ];
    known
        .iter()
        .find(|(name, _)| in_path(name))
        .map(|(name, flags)| {
            std::iter::once(name.to_string()).chain(flags.iter().map(|f| f.to_string())).collect()
        })
        .unwrap_or_else(|| vec!["xterm".into(), "-e".into()])
}

/// Starts the application (or one of its actions) detached from the shell.
/// Starts `command` in a new session, without a controlling terminal.
///
/// A display manager may start the desktop on a VT, which then becomes the controlling
/// terminal of everything launched from it. An interactive shell started by such a program
/// (Electron applications read the login shell's environment this way) is stopped by SIGTTIN
/// when it is not in the terminal's foreground process group, and the program hangs.
///
/// The shell may run at a raised priority (LunaDash's ananicy-cpp rule gives it nice -6);
/// launched programs start at the normal priority instead of inheriting it, or a build
/// started from a terminal would compete with the desktop again.
pub fn detach(command: &mut Command) -> &mut Command {
    // SAFETY: setsid, getpriority and setpriority are async-signal-safe system calls and
    // touch no memory of the parent.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // Raising the nice value back to 0 needs no privilege. getpriority also returns
            // -1 on failure; resetting to 0 then is harmless.
            if libc::getpriority(libc::PRIO_PROCESS, 0) < 0 {
                libc::setpriority(libc::PRIO_PROCESS, 0, 0);
            }
            Ok(())
        })
    }
}

pub fn launch(app: &AppEntry, action: Option<&str>) -> Result<(), LaunchError> {
    let exec = action
        .and_then(|id| app.actions.iter().find(|a| a.id == id))
        .map_or(app.exec.as_str(), |a| a.exec.as_str());
    let mut argv = parse_exec(exec, app);
    if argv.is_empty() {
        return Err(LaunchError::EmptyCommand);
    }
    if app.terminal {
        let mut wrapped = terminal();
        wrapped.append(&mut argv);
        argv = wrapped;
    }
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    detach(&mut command);
    if let Some(home) = std::env::var_os("HOME") {
        command.current_dir(home);
    }
    let mut child = command.spawn().map_err(LaunchError::Spawn)?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> AppEntry {
        AppEntry {
            name: "My App".into(),
            icon: "myapp".into(),
            path: "/a/b.desktop".into(),
            ..Default::default()
        }
    }

    #[test]
    fn removes_file_field_codes() {
        assert_eq!(parse_exec("firefox %u", &app()), vec!["firefox"]);
        assert_eq!(parse_exec("code --new-window %F", &app()), vec!["code", "--new-window"]);
    }

    #[test]
    fn honours_quotes_and_escapes() {
        assert_eq!(
            parse_exec(r#""/opt/My App/run" --title "a \"b\"" 100%%"#, &app()),
            vec!["/opt/My App/run", "--title", "a \"b\"", "100%"]
        );
        assert_eq!(parse_exec(r#"sh -c "echo \$HOME""#, &app()), vec!["sh", "-c", "echo $HOME"]);
    }

    #[test]
    fn expands_name_icon_and_path() {
        assert_eq!(
            parse_exec("app %i --name %c %k", &app()),
            vec!["app", "--icon", "myapp", "--name", "My App", "/a/b.desktop"]
        );
    }
}
