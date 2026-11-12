//! Minimal Rust-only shell: a top bar on every monitor, using `slintlu` as a library.
//!
//! ```sh
//! SLINTLU_RENDERER=vulkan cargo run --example hello_bar
//! ```

use slint::ComponentHandle;
use slintlu::{Anchors, LayerConfig, PlatformConfig, SurfaceRole};

slint::slint! {
    export component Bar inherits Window {
        in property <string> output-name;
        in property <string> renderer;
        in-out property <int> clicks;
        preferred-height: 32px;
        background: #1e1e2ecc;
        HorizontalLayout {
            padding-left: 12px;
            padding-right: 12px;
            spacing: 12px;
            Text { text: "SlintLu · " + root.output-name; color: #cdd6f4; vertical-alignment: center; }
            Rectangle { horizontal-stretch: 1; }
            Text { text: root.renderer; color: #a6e3a1; vertical-alignment: center; }
            Rectangle {
                width: 90px;
                border-radius: 6px;
                background: touch.has-hover ? #45475a : #313244;
                animate background { duration: 150ms; }
                Text { text: "clicks: " + root.clicks; color: #f5c2e7; }
                touch := TouchArea { clicked => { root.clicks += 1; } }
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let renderer = std::env::var("SLINTLU_RENDERER").unwrap_or_default().parse()?;
    let backend = std::env::var("SLINTLU_BACKEND").unwrap_or_default().parse()?;
    slintlu::init(PlatformConfig { renderer, backend })?;

    eprintln!("outputs: {:?}", slintlu::outputs());
    let mut bars = Vec::new();
    for output in slintlu::outputs() {
        let bar = Bar::new()?;
        let window = slintlu::window_for(bar.window()).expect("created by slintlu");
        window.set_role(SurfaceRole::Layer(LayerConfig {
            namespace: "hello-bar".into(),
            anchors: Anchors::TOP_EDGE,
            output: Some(output.id),
            ..LayerConfig::default()
        }));
        bar.set_output_name(output.name.clone().into());
        bar.set_renderer(window.renderer_kind().to_string().into());
        bar.show()?;
        bars.push(bar);
    }
    slint::run_event_loop_until_quit()?;
    Ok(())
}
