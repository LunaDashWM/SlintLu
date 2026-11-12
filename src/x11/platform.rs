//! The Slint [`Platform`] implementation for X11 and its event loop.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use slint::platform::{EventLoopProxy, Platform, PlatformError, WindowAdapter};
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{EventLoop, Interest, Mode, PostAction, channel};
use x11rb::connection::Connection;
use x11rb::protocol::xkb::{self, ConnectionExt as _};
use x11rb::xcb_ffi::XCBConnection;

use super::atoms::Atoms;
use super::context::{self, Context, Visual};
use super::events::{self, Input};
use super::keymap::Keymap;
use super::window::X11Window;
use crate::core::proxy::{LoopMessage, Proxy};
use crate::platform::PlatformConfig;
use crate::render::RendererKind;

struct LoopState {
    input: Input,
    quit: bool,
}

/// The Slint platform for X11. Usually installed with [`crate::init`].
pub struct X11Platform {
    ctx: Rc<Context>,
    event_loop: RefCell<Option<EventLoop<'static, LoopState>>>,
    state: RefCell<Option<LoopState>>,
    sender: channel::Sender<LoopMessage>,
}

/// Reports key repeats as presses without releases instead of release/press pairs.
fn enable_detectable_auto_repeat(conn: &XCBConnection) {
    let enabled = conn
        .xkb_use_extension(1, 0)
        .ok()
        .and_then(|c| c.reply().ok())
        .filter(|r| r.supported)
        .and_then(|_| {
            let flag = xkb::PerClientFlag::DETECTABLE_AUTO_REPEAT;
            let none = xkb::BoolCtrl::from(0u32);
            conn.xkb_per_client_flags(xkb::ID::USE_CORE_KBD.into(), flag, flag, none, none, none)
                .ok()
        })
        .and_then(|c| c.reply().ok())
        .is_some_and(|r| r.value.contains(xkb::PerClientFlag::DETECTABLE_AUTO_REPEAT));
    if !enabled {
        log::debug!("detectable auto-repeat is unavailable: repeats look like separate presses");
    }
}

impl X11Platform {
    /// Connects to the X server named by `$DISPLAY`.
    pub fn new(config: PlatformConfig) -> Result<Self, PlatformError> {
        let (conn, screen) = XCBConnection::connect(None)
            .map_err(|e| format!("cannot connect to the X server: {e}"))?;
        let root_screen = &conn.setup().roots[screen];
        let root = root_screen.root;
        let atoms = Atoms::new(&conn)
            .map_err(|e| e.to_string())
            .and_then(|cookie| cookie.reply().map_err(|e| e.to_string()))
            .map_err(|e| format!("cannot intern atoms: {e}"))?;
        let visual = Visual::choose(&conn, root_screen)?;
        let keymap =
            Keymap::load(&conn).map_err(|e| format!("cannot read the keyboard mapping: {e}"))?;
        enable_detectable_auto_repeat(&conn);
        let randr = super::output::watch(&conn, root);

        match config.renderer {
            RendererKind::Software => {}
            RendererKind::Auto => log::info!("X11 windows use the software renderer"),
            other => log::warn!("the {other} renderer is not supported on X11 yet, using software"),
        }

        // The event loop watches a duplicate so that it never closes the connection's own fd.
        let fd = rustix::io::dup(&conn).map_err(|e| format!("cannot watch the X connection: {e}"))?;

        let ctx = Rc::new(Context {
            conn,
            screen,
            root,
            atoms,
            visual,
            randr,
            keymap: RefCell::new(keymap),
            windows: Default::default(),
            outputs: Default::default(),
            output_listeners: Default::default(),
        });
        super::output::refresh(&ctx);

        let event_loop: EventLoop<'static, LoopState> =
            EventLoop::try_new().map_err(|e| format!("cannot create event loop: {e}"))?;
        // Events are read in `run_event_loop`; this source only wakes the loop up.
        event_loop
            .handle()
            .insert_source(Generic::new(fd, Interest::READ, Mode::Level), |_, _, _| {
                Ok(PostAction::Continue)
            })
            .map_err(|e| format!("cannot watch the X connection: {e}"))?;

        let (sender, receiver) = channel::channel::<LoopMessage>();
        event_loop
            .handle()
            .insert_source(receiver, |event, _, state: &mut LoopState| match event {
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
            state: RefCell::new(Some(LoopState { input: Input::default(), quit: false })),
            sender,
        })
    }

    /// Handles every event the connection has already received or can read without waiting.
    /// Returns whether there were any.
    fn handle_pending_events(&self, state: &mut LoopState) -> Result<bool, PlatformError> {
        let mut any = false;
        while let Some(event) = self
            .ctx
            .conn
            .poll_for_event()
            .map_err(|e| PlatformError::from(format!("lost the X connection: {e}")))?
        {
            events::handle(&self.ctx, &mut state.input, event);
            any = true;
        }
        Ok(any)
    }
}

impl Platform for X11Platform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        let window = X11Window::new(self.ctx.clone())?;
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
            if let Err(e) = self.handle_pending_events(&mut state) {
                break Err(e);
            }
            let next_frame = self.ctx.render_dirty_windows();
            if let Err(e) = self.ctx.conn.flush() {
                break Err(PlatformError::from(format!("lost the X connection: {e}")));
            }
            if state.quit {
                break Ok(());
            }
            // Slint callbacks and drawing may have queued events while waiting for replies;
            // the socket would not wake the loop up for those.
            match self.handle_pending_events(&mut state) {
                Ok(true) => continue,
                Ok(false) => {}
                Err(e) => break Err(e),
            }
            let timer = slint::platform::duration_until_next_timer_update();
            let frame = next_frame.map(|at| at.saturating_duration_since(Instant::now()));
            let timeout = match (timer, frame) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            if let Err(e) = event_loop.dispatch(timeout, &mut state) {
                break Err(PlatformError::from(format!("event loop error: {e}")));
            }
            if state.quit {
                break Ok(());
            }
        };

        *self.event_loop.borrow_mut() = Some(event_loop);
        *self.state.borrow_mut() = Some(state);
        result
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy { sender: self.sender.clone() }))
    }
}
