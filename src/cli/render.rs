//! `slintlu render`: plays a component on a clock of its own and writes each frame as a PNG,
//! for animations shown where Slint cannot run (a Plymouth boot splash).

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, PlatformError, WindowAdapter};
use slint_interpreter::ComponentHandle;

pub struct RenderOptions {
    pub path: PathBuf,
    pub component: Option<String>,
    pub out: PathBuf,
    pub prefix: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub seconds: f64,
}

/// The time Slint sees: set per frame, so animations land on exact frame times.
struct FrameClock {
    window: Rc<MinimalSoftwareWindow>,
    now: Rc<Cell<Duration>>,
}

impl Platform for FrameClock {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        self.now.get()
    }
}

pub fn render(options: RenderOptions) -> Result<(), String> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    let now = Rc::new(Cell::new(Duration::ZERO));
    slint::platform::set_platform(Box::new(FrameClock { window: window.clone(), now: now.clone() }))
        .map_err(|e| e.to_string())?;

    let compiler = slint_interpreter::Compiler::default();
    let result = pollster::block_on(compiler.build_from_path(&options.path));
    if result.has_errors() {
        slint_interpreter::print_diagnostics(&result.diagnostics().collect::<Vec<_>>());
        return Err(format!("{} has errors", options.path.display()));
    }
    let name = match &options.component {
        Some(name) => name.clone(),
        None => result
            .component_names()
            .last()
            .ok_or_else(|| format!("{} exports no component", options.path.display()))?
            .to_owned(),
    };
    let definition =
        result.component(&name).ok_or_else(|| format!("no component named {name}"))?;
    let instance = definition.create().map_err(|e| e.to_string())?;
    window.set_size(slint::PhysicalSize::new(options.width, options.height));
    instance.show().map_err(|e| e.to_string())?;

    std::fs::create_dir_all(&options.out)
        .map_err(|e| format!("{}: {e}", options.out.display()))?;
    let frames = (options.seconds * options.fps).round().max(1.0) as usize;
    let (width, height) = (options.width as usize, options.height as usize);
    let mut background = None;
    for frame in 0..frames {
        now.set(Duration::from_secs_f64(frame as f64 / options.fps));
        slint::platform::update_timers_and_animations();
        window.request_redraw();
        let mut pixels = vec![PremultipliedRgbaColor::default(); width * height];
        window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, width);
        });
        if frame == 0 {
            background = pixels.first().filter(|p| p.alpha == 255).copied();
        }
        let path = options.out.join(format!("{}{frame:04}.png", options.prefix));
        save(&path, &pixels, options.width, options.height)?;
    }
    let background = background
        .map(|p| format!("\"#{:02x}{:02x}{:02x}\"", p.red, p.green, p.blue))
        .unwrap_or_else(|| "null".into());
    println!(
        "{{\"frames\":{frames},\"width\":{},\"height\":{},\"fps\":{},\"background\":{background}}}",
        options.width, options.height, options.fps
    );
    Ok(())
}

fn save(path: &Path, pixels: &[PremultipliedRgbaColor], width: u32, height: u32) -> Result<(), String> {
    let unpremultiply = |c: u8, a: u8| if a == 0 { 0 } else { (c as u32 * 255 / a as u32).min(255) as u8 };
    let rgba: Vec<u8> = pixels
        .iter()
        .flat_map(|p| {
            [unpremultiply(p.red, p.alpha), unpremultiply(p.green, p.alpha), unpremultiply(p.blue, p.alpha), p.alpha]
        })
        .collect();
    image::save_buffer(path, &rgba, width, height, image::ColorType::Rgba8)
        .map_err(|e| format!("{}: {e}", path.display()))
}
