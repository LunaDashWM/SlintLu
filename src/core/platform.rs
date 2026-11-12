//! The Slint [`Platform`] implementation and its event loop.

use std::cell::RefCell;
use std::rc::Rc;

use slint::platform::{EventLoopProxy, Platform, PlatformError, WindowAdapter};
use smithay_client_toolkit::compositor::CompositorState;
use smithay_client_toolkit::reexports::calloop::{EventLoop, channel};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::Connection;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::session_lock::SessionLockState;
use smithay_client_toolkit::shell::wlr_layer::LayerShell;
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shm::Shm;

use super::context::{self, Context};
use super::proxy::{LoopMessage, Proxy};
use super::state::State;
use crate::platform::PlatformConfig;
use crate::window::WaylandWindow;

/// The Slint platform for Wayland. Usually installed with [`crate::init`].
pub struct WaylandPlatform {
    ctx: Rc<Context>,
    event_loop: RefCell<Option<EventLoop<'static, State>>>,
    state: RefCell<Option<State>>,
    sender: channel::Sender<LoopMessage>,
}

impl WaylandPlatform {
    /// Connects to the compositor named by `$WAYLAND_DISPLAY` and binds the globals needed by
    /// shells. Optional protocols (layer-shell, xdg-shell, cursor-shape) degrade gracefully.
    pub fn new(config: PlatformConfig) -> Result<Self, PlatformError> {
        let conn = Connection::connect_to_env()
            .map_err(|e| format!("cannot connect to the Wayland compositor: {e}"))?;
        let (globals, mut event_queue) =
            registry_queue_init::<State>(&conn).map_err(|e| format!("wayland registry: {e}"))?;
        let qh = event_queue.handle();

        let compositor = CompositorState::bind(&globals, &qh)
            .map_err(|e| format!("wl_compositor is not available: {e}"))?;
        let shm = Shm::bind(&globals, &qh).map_err(|e| format!("wl_shm is not available: {e}"))?;
        let layer_shell = LayerShell::bind(&globals, &qh)
            .inspect_err(|e| log::warn!("zwlr_layer_shell_v1 is not available: {e}"))
            .ok();
        let xdg_shell = XdgShell::bind(&globals, &qh)
            .inspect_err(|e| log::warn!("xdg_wm_base is not available: {e}"))
            .ok();

        let session_lock_state = globals
            .contents()
            .with_list(|list| list.iter().any(|g| g.interface == "ext_session_lock_manager_v1"))
            .then(|| SessionLockState::new(&globals, &qh));
        if session_lock_state.is_none() {
            log::warn!("ext_session_lock_manager_v1 is not available: lock screens are disabled");
        }

        let ctx = Rc::new(Context {
            conn: conn.clone(),
            qh: qh.clone(),
            compositor,
            layer_shell,
            xdg_shell,
            shm,
            clipboard: crate::clipboard::ClipboardState::new(&globals, &qh),
            clipboard_history: crate::clipboard::ClipboardHistory::new(&globals, &qh),
            fractional_scaling: crate::window::scale::FractionalScaling::bind(&globals, &qh),
            idle: crate::idle::IdleState::new(&globals, &qh),
            renderer: config.renderer,
            failed_renderers: Default::default(),
            globals: globals
                .contents()
                .with_list(|list| list.iter().map(|g| g.interface.clone()).collect()),
            windows: Default::default(),
            outputs: Default::default(),
            output_listeners: Default::default(),
            session_lock_state,
            session_lock: Default::default(),
            lock_listeners: Default::default(),
            #[cfg(feature = "opengl")]
            egl: Default::default(),
            #[cfg(feature = "vulkan")]
            gpu: Default::default(),
        });

        let event_loop: EventLoop<'static, State> =
            EventLoop::try_new().map_err(|e| format!("cannot create event loop: {e}"))?;
        let mut state = State::new(ctx.clone(), &globals, &qh, event_loop.handle());

        // Two round trips: one for the globals' initial events (outputs, seats), one for the
        // output details (names, modes) those trigger.
        for _ in 0..2 {
            event_queue
                .roundtrip(&mut state)
                .map_err(|e| format!("wayland roundtrip failed: {e}"))?;
        }
        crate::output::refresh(&ctx, &state.output_state);
        // Seats present at startup do not go through `SeatHandler::new_seat`.
        for seat in state.seat_state.seats() {
            ctx.clipboard.add_seat(&seat, &qh);
            ctx.clipboard_history.add_seat(&seat, &qh);
            ctx.idle.set_seat(&seat);
        }

        WaylandSource::new(conn, event_queue)
            .insert(event_loop.handle())
            .map_err(|e| format!("cannot watch the wayland socket: {e}"))?;

        let (sender, receiver) = channel::channel::<LoopMessage>();
        event_loop
            .handle()
            .insert_source(receiver, |event, _, state: &mut State| match event {
                channel::Event::Msg(LoopMessage::Invoke(f)) => f(),
                channel::Event::Msg(LoopMessage::Quit) | channel::Event::Closed => {
                    state.quit = true
                }
            })
            .map_err(|e| format!("cannot install the event loop channel: {e}"))?;

        context::install(&ctx);

        Ok(Self {
            ctx,
            event_loop: RefCell::new(Some(event_loop)),
            state: RefCell::new(Some(state)),
            sender,
        })
    }

    /// Whether the compositor implements `wlr-layer-shell`.
    pub fn supports_layer_shell(&self) -> bool {
        self.ctx.layer_shell.is_some()
    }
}

impl Platform for WaylandPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        let window = WaylandWindow::new(self.ctx.clone())?;
        Ok(window)
    }

    fn run_event_loop(&self) -> Result<(), PlatformError> {
        let mut event_loop = self
            .event_loop
            .borrow_mut()
            .take()
            .ok_or_else(|| PlatformError::from("the event loop is already running"))?;
        let mut state = self.state.borrow_mut().take().expect("state lives with the event loop");
        state.quit = false;

        let result = loop {
            slint::platform::update_timers_and_animations();
            self.ctx.render_dirty_windows();
            if let Err(e) = self.ctx.conn.flush() {
                break Err(PlatformError::from(format!("lost the wayland connection: {e}")));
            }
            if state.quit {
                break Ok(());
            }
            let timeout = slint::platform::duration_until_next_timer_update();
            if let Err(e) = event_loop.dispatch(timeout, &mut state) {
                break Err(PlatformError::from(format!("event loop error: {e}")));
            }
            if state.quit {
                break Ok(());
            }
        };

        *self.event_loop.borrow_mut() = Some(event_loop);
        *self.state.borrow_mut() = Some(state);
        self.ctx.release_graphics();
        result
    }

    fn set_clipboard_text(&self, text: &str, clipboard: slint::platform::Clipboard) {
        self.ctx.clipboard.set_text(text, clipboard, &self.ctx.qh);
        let _ = self.ctx.conn.flush();
    }

    fn clipboard_text(&self, clipboard: slint::platform::Clipboard) -> Option<String> {
        self.ctx.clipboard.text(clipboard, &self.ctx.conn)
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy { sender: self.sender.clone() }))
    }
}
