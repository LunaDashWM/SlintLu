//! The `Session` global: locking with a shell-defined lock screen and unlocking with PAM.

use std::cell::Cell;
use std::rc::Rc;

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use slint_interpreter::Value;
use slintlu::dbus::accounts;
use slintlu::dbus::power::{Power, SessionEvent};
use slintlu::security::{self, LockEvent};

use super::worker::spawn_watcher;
use crate::scripting::bridge::{bool_arg, string, string_arg};
use crate::scripting::{ShellHost, host};

thread_local! {
    static AUTHENTICATING: Cell<bool> = const { Cell::new(false) };
    static LOCK_BEFORE_SLEEP: Cell<bool> = const { Cell::new(true) };
}

fn set_locked_hint(locked: bool) {
    std::thread::spawn(move || {
        if let Ok(power) = Power::connect()
            && let Err(e) = power.set_locked_hint(locked)
        {
            log::debug!("cannot set the logind locked hint: {e}");
        }
    });
}

/// Locks the session with the shell's lock screen.
pub fn lock(host: &ShellHost) {
    if security::session_locked() {
        return;
    }
    if !host.has_lock_screen() {
        // Locking without a lock screen would leave the user stuck on a blank screen.
        return log::error!("cannot lock: the shell has no component inheriting LockWindow");
    }
    if let Err(e) = security::lock_session() {
        return log::error!("{e}");
    }
    host.set_global("Session", "message", string(""));
    host.show_lock_screens();
}

fn unlock(host: &ShellHost) {
    host.hide_lock_screens();
    security::unlock_session();
    host.set_global("Session", "locked", Value::Bool(false));
    host.set_global("Session", "authenticating", Value::Bool(false));
    host.set_global("Session", "message", string(""));
    set_locked_hint(false);
}

fn authenticate(host: &ShellHost, password: String) {
    if !security::session_locked() || AUTHENTICATING.replace(true) {
        return;
    }
    host.set_global("Session", "authenticating", Value::Bool(true));
    host.set_global("Session", "message", string(""));
    std::thread::spawn(move || {
        let result = security::authenticate(
            &security::default_service(),
            &security::current_user(),
            &password,
        );
        drop(password);
        host::post(move |host| {
            AUTHENTICATING.set(false);
            host.set_global("Session", "authenticating", Value::Bool(false));
            match result {
                Ok(()) => unlock(host),
                Err(e) => {
                    log::info!("unlock failed: {e}");
                    host.set_global("Session", "message", string(e.to_string()));
                }
            }
        });
    });
}

fn home() -> std::path::PathBuf {
    std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_else(|| "/".into())
}

type Pixels = SharedPixelBuffer<Rgba8Pixel>;

fn pixels(picture: &image::RgbaImage) -> Pixels {
    SharedPixelBuffer::clone_from_slice(picture.as_raw(), picture.width(), picture.height())
}

/// Decodes a picture off the UI thread, as it is and cut to a circle (Slint's `clip` keeps
/// square corners). Pixels, not a path: Slint caches images by path and would keep showing
/// the old ~/.face.
fn decode(path: &std::path::Path) -> Option<(Pixels, Pixels)> {
    let picture =
        accounts::open_picture(path).map_err(|e| log::warn!("account picture: {e}")).ok()?;
    let side = picture.width().min(picture.height()).max(1);
    let mut round =
        picture.resize_to_fill(side, side, image::imageops::FilterType::Triangle).into_rgba8();
    slintlu::files::round_corners(&mut round, [side as f32 / 2.0; 4]);
    Some((pixels(&picture.into_rgba8()), pixels(&round)))
}

/// Reads the account's name and picture on a thread, then shows them.
fn load_account() {
    std::thread::spawn(|| {
        let account = accounts::load(&security::current_user(), &home());
        let pictures = account.icon.as_deref().and_then(decode);
        host::post(move |host| {
            host.set_global("Session", "real-name", string(&account.real_name));
            let (avatar, round) = pictures
                .map(|(avatar, round)| (Image::from_rgba8(avatar), Image::from_rgba8(round)))
                .unwrap_or_default();
            host.set_global("Session", "avatar", Value::Image(avatar));
            host.set_global("Session", "avatar-round", Value::Image(round));
        });
    });
}

pub fn install(host: &Rc<ShellHost>) {
    host.set_global("Session", "user", string(security::current_user()));
    load_account();
    host.on_callback(
        "Session",
        "set-avatar",
        Rc::new(|args| {
            let Some(path) = string_arg(args, 0).filter(|p| !p.is_empty()) else {
                return Value::Void;
            };
            std::thread::spawn(move || {
                let result = accounts::set_icon(&security::current_user(), &home(), path.as_ref());
                host::post(move |host| {
                    let error = result.err().unwrap_or_default();
                    host.set_global("Session", "avatar-error", string(&error));
                });
                load_account();
            });
            Value::Void
        }),
    );

    let weak = Rc::downgrade(host);
    security::on_lock_event(move |event| {
        let Some(host) = weak.upgrade() else { return };
        match event {
            LockEvent::Locked => {
                host.set_global("Session", "locked", Value::Bool(true));
                set_locked_hint(true);
            }
            LockEvent::Finished => {
                log::warn!("the compositor ended the session lock");
                host.hide_lock_screens();
                host.set_global("Session", "locked", Value::Bool(false));
            }
        }
    });

    host.on_callback(
        "Session",
        "lock",
        Rc::new(|_| {
            // Not from inside a window's own callback: windows are created while locking.
            host::post(|host| lock(host));
            Value::Void
        }),
    );
    let weak = Rc::downgrade(host);
    host.on_callback(
        "Session",
        "set-lock-before-sleep",
        Rc::new(move |args| {
            if let (Some(enabled), Some(host)) = (bool_arg(args, 0), weak.upgrade()) {
                LOCK_BEFORE_SLEEP.with(|l| l.set(enabled));
                host.set_global("Session", "lock-before-sleep", Value::Bool(enabled));
            }
            Value::Void
        }),
    );
    host.on_callback(
        "Session",
        "authenticate",
        Rc::new(|args| {
            let password = string_arg(args, 0).unwrap_or_default();
            host::post(move |host| authenticate(host, password));
            Value::Void
        }),
    );

    // logind: `loginctl lock-session`, idle daemons and suspend.
    if let Ok(power) = Power::connect() {
        spawn_watcher("session", move || {
            power
                .watch(|event| match event {
                    SessionEvent::Lock => host::post(|host| lock(host)),
                    SessionEvent::Unlock => host::post(|host| {
                        if security::session_locked() {
                            unlock(host);
                        }
                    }),
                    SessionEvent::Sleep(true) => host::post(|host| {
                        if LOCK_BEFORE_SLEEP.with(|l| l.get()) {
                            lock(host);
                        }
                    }),
                    SessionEvent::Sleep(false) => {}
                })
                .map_err(std::io::Error::other)
        });
    }
}
