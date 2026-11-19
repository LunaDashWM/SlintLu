//! The user's account (AccountsService): display name and picture.

use std::path::{Path, PathBuf};

use zbus::Connection;
use zbus::OwnedObjectPath;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Account {
    /// The full name, or "" when the account has none.
    pub real_name: String,
    /// The picture file, when there is one.
    pub icon: Option<PathBuf>,
}

// AccountsService over the system bus, one blocking call at a time (callers use a thread).
fn system() -> zbus::Result<Connection> {
    zbus::block_on(Connection::system())
}

fn user_path(connection: &Connection, user: &str) -> zbus::Result<OwnedObjectPath> {
    zbus::block_on(async {
        let reply = connection
            .call_method(
                Some("org.freedesktop.Accounts"),
                "/org/freedesktop/Accounts",
                Some("org.freedesktop.Accounts"),
                "FindUserByName",
                &(user,),
            )
            .await?;
        reply.body().deserialize()
    })
}

fn property(connection: &Connection, path: &OwnedObjectPath, name: &str) -> zbus::Result<String> {
    zbus::block_on(async {
        let reply = connection
            .call_method(
                Some("org.freedesktop.Accounts"),
                path,
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &("org.freedesktop.Accounts.User", name),
            )
            .await?;
        let value: zbus::OwnedVariant = reply.body().deserialize()?;
        String::try_from(value)
    })
}

/// The account of `user`, falling back to `~/.face` for the picture.
pub fn load(user: &str, home: &Path) -> Account {
    let mut account = Account::default();
    if let Ok(connection) = system()
        && let Ok(path) = user_path(&connection, user)
    {
        account.real_name = property(&connection, &path, "RealName").unwrap_or_default();
        account.icon = property(&connection, &path, "IconFile")
            .ok()
            .map(PathBuf::from)
            .filter(|p| p.is_file());
    }
    if account.icon.is_none() {
        account.icon = [".face", ".face.icon"].iter().map(|n| home.join(n)).find(|p| p.is_file());
    }
    account
}

/// Decodes a picture by its contents: account pictures (`~/.face`, AccountsService's icons)
/// have no extension to tell the format.
pub fn open_picture(path: &Path) -> Result<image::DynamicImage, String> {
    image::ImageReader::open(path)
        .and_then(|reader| reader.with_guessed_format())
        .map_err(|e| format!("{}: {e}", path.display()))?
        .decode()
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Makes `image` the account picture: a 256 px square PNG at `~/.face` (read by display
/// managers), which AccountsService copies for the login screen.
pub fn set_icon(user: &str, home: &Path, image: &Path) -> Result<PathBuf, String> {
    let picture = open_picture(image)?;
    let side = picture.width().min(picture.height());
    let square =
        picture.crop_imm((picture.width() - side) / 2, (picture.height() - side) / 2, side, side);
    let face = home.join(".face");
    square
        .resize_exact(256, 256, image::imageops::FilterType::Lanczos3)
        .save_with_format(&face, image::ImageFormat::Png)
        .map_err(|e| format!("{}: {e}", face.display()))?;
    // Without AccountsService the picture is still at ~/.face.
    if let Ok(connection) = system()
        && let Ok(path) = user_path(&connection, user)
        && let Err(e) = zbus::block_on(connection.call_method(
            Some("org.freedesktop.Accounts"),
            &path,
            Some("org.freedesktop.Accounts.User"),
            "SetIconFile",
            &(face.to_string_lossy().as_ref(),),
        ))
    {
        log::warn!("accounts: the login screen keeps its picture: {e}");
    }
    Ok(face)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_without_an_extension_decode() {
        let dir = tempfile::tempdir().unwrap();
        let face = dir.path().join(".face");
        image::RgbaImage::from_pixel(4, 3, image::Rgba([10, 20, 30, 255]))
            .save_with_format(&face, image::ImageFormat::Png)
            .unwrap();
        let picture = open_picture(&face).unwrap();
        assert_eq!((picture.width(), picture.height()), (4, 3));
        assert!(open_picture(&dir.path().join("missing")).is_err());
    }
}
