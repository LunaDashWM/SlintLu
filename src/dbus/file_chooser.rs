//! Choosing a file or folder through the XDG desktop portal's FileChooser.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::StreamExt;
use zbus::{Connection, OwnedObjectPath, OwnedVariant, Variant, proxy};

use crate::util::uri::path_from_path_or_uri;

#[proxy(
    interface = "org.freedesktop.portal.FileChooser",
    default_service = "org.freedesktop.portal.Desktop",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait FileChooser {
    fn open_file(
        &self,
        parent_window: &str,
        title: &str,
        options: HashMap<&str, Variant<'_>>,
    ) -> zbus::Result<OwnedObjectPath>;
}

#[proxy(
    interface = "org.freedesktop.portal.Request",
    default_service = "org.freedesktop.portal.Desktop"
)]
trait Request {
    #[zbus(signal)]
    fn response(&self, response: u32, results: HashMap<String, OwnedVariant>) -> zbus::Result<()>;
}

/// What to ask the user for.
#[derive(Clone, Debug, Default)]
pub struct ChooseOptions {
    pub title: String,
    /// Opaque portal parent identifier (`x11:<window>` or a Wayland activation token).
    pub parent_window: String,
    /// xdg-activation token, used to raise the chooser above the requesting window on Wayland.
    pub activation_token: Option<String>,
    /// Lowercase extensions without dots; empty accepts every file.
    pub extensions: Vec<String>,
    /// Choose a folder instead of a file.
    pub directory: bool,
    /// The folder the dialog starts in.
    pub folder: Option<PathBuf>,
}

pub struct ChooseRequest {
    pub options: ChooseOptions,
    pub on_selected: Arc<dyn Fn(Option<PathBuf>) + Send + Sync>,
}

/// Requests an activation token from the Wayland compositor, if available, then opens the
/// portal chooser. X11 gets an explicit parent XID.
pub fn request(request: ChooseRequest) {
    let ChooseRequest { options, on_selected } = request;
    if let Some(ctx) = crate::core::context::current()
        && let Some(activation) = ctx.activation.as_ref()
    {
        let surface = ctx
            .live_windows()
            .into_iter()
            .rev()
            .find(|window| {
                window.is_shown()
                    && matches!(
                        window.role(),
                        crate::shell::config::SurfaceRole::Toplevel { .. }
                    )
            })
            .map(|window| window.wl_surface().clone());
        if let Some(surface) = surface {
            use smithay_client_toolkit::activation::RequestData;
            activation.request_token(
                &ctx.qh,
                RequestData {
                    app_id: Some("slintlu".to_owned()),
                    seat_and_serial: None,
                    surface: Some(surface),
                    udata: ChooseRequest {
                        options: options.clone(),
                        on_selected: on_selected.clone(),
                    },
                },
            );
            return;
        }
    }

    std::thread::spawn(move || match choose(&options) {
        Ok(path) => on_selected(path),
        Err(e) => log::warn!("file chooser: {e}"),
    });
}

/// The portal's glob patterns for `extensions`, in both cases (the patterns are case
/// sensitive).
fn patterns(extensions: &[String]) -> Vec<(u32, String)> {
    extensions
        .iter()
        .flat_map(|e| [format!("*.{e}"), format!("*.{}", e.to_ascii_uppercase())])
        .map(|pattern| (0, pattern))
        .collect()
}

/// The object path of the request a call with `token` creates (so its response signal can be
/// subscribed before the call returns).
fn request_path(unique_name: &str, token: &str) -> String {
    let sender = unique_name.trim_start_matches(':').replace('.', "_");
    format!("/org/freedesktop/portal/desktop/request/{sender}/{token}")
}

fn folder_bytes(folder: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    let mut bytes = folder.as_os_str().as_bytes().to_vec();
    bytes.push(0);
    bytes
}

/// Shows the portal's file chooser and blocks until the user picks something. `Ok(None)` when
/// the dialog was cancelled.
pub fn choose(options: &ChooseOptions) -> zbus::Result<Option<PathBuf>> {
    zbus::block_on(async {
        let connection = Connection::session().await?;
        let token = format!("slintlu{}", std::process::id() ^ rand_suffix());
        let unique = connection.unique_name().map(|n| n.to_string()).unwrap_or_default();
        let request = RequestProxy::builder(&connection)
            .path(zbus::ObjectPath::try_from(request_path(&unique, &token))?)
            .build()
            .await?;
        let mut responses = request.receive_response().await?;

        let mut call: HashMap<&str, Variant<'_>> = HashMap::new();
        call.insert("handle_token", Variant::from(token.as_str()));
        call.insert("modal", Variant::from(true));
        call.insert("directory", Variant::from(options.directory));
        if !options.extensions.is_empty() && !options.directory {
            let filter = (options.title.clone(), patterns(&options.extensions));
            call.insert("filters", Variant::from(vec![filter]));
        }
        if let Some(folder) = options.folder.as_deref().filter(|f| f.is_dir()) {
            call.insert("current_folder", Variant::from(folder_bytes(folder)));
        }
        let chooser = FileChooserProxy::new(&connection).await?;
        if let Some(token) = options.activation_token.as_deref() {
            call.insert("activation_token", Variant::from(token));
        }
        chooser.open_file(&options.parent_window, &options.title, call).await?;

        let Some(signal) = responses.next().await else {
            return Err(zbus::Error::Failure("the portal did not answer".into()));
        };
        let args = signal.args()?;
        if args.response != 0 {
            return Ok(None);
        }
        let uris = args
            .results
            .get("uris")
            .and_then(|v| <Vec<String>>::try_from(v.clone()).ok())
            .unwrap_or_default();
        Ok(uris.first().map(|uri| path_from_path_or_uri(uri)))
    })
}

/// A little entropy for request tokens, so concurrent dialogs do not share one.
fn rand_suffix() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_request_paths_and_patterns() {
        assert_eq!(
            request_path(":1.42", "slintlu7"),
            "/org/freedesktop/portal/desktop/request/1_42/slintlu7"
        );
        assert_eq!(
            patterns(&["png".into()]),
            vec![(0, "*.png".to_owned()), (0, "*.PNG".to_owned())]
        );
        assert_eq!(folder_bytes(Path::new("/a")), b"/a\0".to_vec());
    }
}
