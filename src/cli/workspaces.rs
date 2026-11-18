//! `slintlu workspaces`: list the workspaces and windows the compositor reports.

use std::sync::mpsc;
use std::time::Duration;

use slintlu::compositors::{self, Compositor};

pub fn workspaces() -> Result<(), String> {
    let compositor = Compositor::detect();
    let backend = compositors::backend(compositor)
        .ok_or_else(|| format!("{compositor}: no workspace support (IPC or standard protocols)"))?;
    // `watch` blocks for the session's lifetime: take its first snapshot and leave.
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = backend.watch(&mut |state| {
            let _ = sender.send(state.clone());
        });
    });
    let mut state = receiver
        .recv_timeout(Duration::from_secs(3))
        .map_err(|_| "the compositor reported nothing within 3 seconds".to_owned())?;
    // A compositor seen through several connections completes its picture in a few updates.
    while let Ok(newer) = receiver.recv_timeout(Duration::from_millis(1000)) {
        state = newer;
    }
    println!("compositor: {compositor}");
    for ws in &state.workspaces {
        println!(
            "workspace {:>2} {:<12} {}{}windows {}",
            ws.index,
            if ws.name.is_empty() { "-" } else { &ws.name },
            if ws.active { "active " } else { "" },
            if ws.urgent { "urgent " } else { "" },
            ws.windows
        );
    }
    for window in &state.windows {
        println!(
            "window {:>4} {}{} — {}",
            window.id,
            if window.focused { "* " } else { "  " },
            window.app_id,
            window.title
        );
    }
    Ok(())
}
