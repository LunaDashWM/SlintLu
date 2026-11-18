//! Application launcher: desktop entries, fuzzy search ranked by usage, and launching.

mod command_line;
mod entry;
mod exec;
mod history;
mod scan;
mod search;

pub use command_line::{CommandLineError, split_command_line};
pub use entry::{AppAction, AppEntry};
pub use exec::{LaunchError, detach, launch, parse_exec};
pub use history::LaunchHistory;
pub use scan::{application_dirs, scan};
pub use search::search;
