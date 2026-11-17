//! `slintlu outputs`: list monitors.

use slintlu::{PlatformConfig, RendererKind};

pub fn outputs() -> Result<(), String> {
    slintlu::init(PlatformConfig { renderer: RendererKind::Software, ..Default::default() })
        .map_err(|e| e.to_string())?;
    for o in slintlu::outputs() {
        println!(
            "{:<10} {}x{} @ {},{}  scale {}  {}",
            o.name, o.width, o.height, o.x, o.y, o.scale_factor, o.description
        );
    }
    Ok(())
}
