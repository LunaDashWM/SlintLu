//! Desktop services over D-Bus: the user account (AccountsService), a notification daemon, media players (MPRIS), network state
//! (NetworkManager), power actions (logind), appearance settings and the file chooser (XDG
//! desktop portal).
//!
//! Each service runs on its own thread with [`zbus::block_on`] and reports plain Rust values
//! through a callback, so it can drive Slint globals as well as anything else.

pub mod accounts;
pub mod bluetooth;
pub mod file_chooser;
mod markup;
pub mod mpris;
pub mod network;
pub mod notifications;
pub mod portal;
pub mod power;
pub mod tray;
