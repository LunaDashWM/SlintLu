//! Colour themes: built-in palettes, a `theme.json` in the shell's directory, pywal colours and
//! automatic light / dark switching, and the installed fonts.

mod config;
pub mod fonts;
mod palette;
mod pywal;

pub use config::{ThemeConfig, choose_palette, resolve, uses_dark_slot};
pub use palette::{Color, Palette, preset, presets};
