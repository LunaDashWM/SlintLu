//! Files for shells: listing directories (e.g. wallpapers) and cached thumbnails.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::util::xdg;

/// A file in a listed directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

/// Extensions of images [`decode`] and [`thumbnail`] read.
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp", "gif"];
/// Extensions of videos [`thumbnail`] reads (through ffmpeg).
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "webm", "mkv", "mov", "m4v"];

/// What a path is, judging by the file system and its extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Missing,
    Directory,
    Image,
    Video,
    Other,
}

impl FileKind {
    pub fn of(path: &Path) -> Self {
        if path.is_dir() {
            return Self::Directory;
        }
        if !path.exists() {
            return Self::Missing;
        }
        let extension =
            path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
        if IMAGE_EXTENSIONS.contains(&extension.as_str()) {
            Self::Image
        } else if VIDEO_EXTENSIONS.contains(&extension.as_str()) {
            Self::Video
        } else {
            Self::Other
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Missing => "",
            Self::Directory => "directory",
            Self::Image => "image",
            Self::Video => "video",
            Self::Other => "file",
        }
    }
}

/// An XDG user directory ("pictures", "videos", "downloads", "documents", "music", "desktop")
/// from `user-dirs.dirs`, or the home directory for "home". `None` when it is not configured
/// or does not exist.
pub fn user_dir(name: &str) -> Option<PathBuf> {
    let home = xdg::home_dir();
    if name.eq_ignore_ascii_case("home") {
        return Some(home);
    }
    let key = format!("XDG_{}_DIR", name.to_ascii_uppercase());
    let text = std::fs::read_to_string(xdg::config_home().join("user-dirs.dirs")).ok()?;
    let value = text.lines().find_map(|line| {
        let (k, v) = line.trim().split_once('=')?;
        (k == key).then(|| v.trim().trim_matches('"').to_owned())
    })?;
    let path = match value.strip_prefix("$HOME") {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None => PathBuf::from(value),
    };
    (path.is_dir() && path != home).then_some(path)
}

/// Decodes an image of any [`IMAGE_EXTENSIONS`] format. Blocking.
pub fn decode(path: &Path) -> Result<image::RgbaImage, String> {
    image::open(path).map(|i| i.into_rgba8()).map_err(|e| format!("{}: {e}", path.display()))
}

/// Expands a leading `~` to the home directory; `file://` URIs become paths.
pub fn expand_home(path: &str) -> PathBuf {
    if let Some(path) = crate::util::uri::file_uri_path(path) {
        return path;
    }
    match path.strip_prefix('~') {
        Some(rest) => xdg::home_dir().join(rest.trim_start_matches('/')),
        None => PathBuf::from(path),
    }
}

/// Entries of `dir`, directories first, then by name. `extensions` (lowercase, without dots)
/// filters files; empty keeps all. Hidden files are skipped.
pub fn list(dir: &Path, extensions: &[&str]) -> Vec<FileEntry> {
    let mut entries: Vec<FileEntry> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            let is_dir = path.is_dir();
            let wanted = is_dir
                || extensions.is_empty()
                || path
                    .extension()
                    .and_then(|x| x.to_str())
                    .is_some_and(|x| extensions.contains(&x.to_ascii_lowercase().as_str()));
            wanted.then_some(FileEntry { path, name, is_dir })
        })
        .collect();
    entries.sort_by(|a, b| {
        b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    entries
}

fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| xdg::home_dir().join(".cache"))
        .join("slintlu/thumbnails")
}

/// Where the thumbnail of `path` at `size` is cached; changes when the file changes.
pub fn thumbnail_path(path: &Path, size: u32) -> PathBuf {
    thumbnail_path_in(&cache_dir(), path, size)
}

fn thumbnail_path_in(cache: &Path, path: &Path, size: u32) -> PathBuf {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let mut hasher = DefaultHasher::new();
    (path, modified, size).hash(&mut hasher);
    cache.join(format!("{:016x}.png", hasher.finish()))
}

/// Creates (or reuses) a PNG thumbnail at most `size` pixels wide or high. Blocking.
pub fn thumbnail(path: &Path, size: u32) -> Result<PathBuf, String> {
    thumbnail_in(&cache_dir(), path, size)
}

fn thumbnail_in(cache: &Path, path: &Path, size: u32) -> Result<PathBuf, String> {
    let target = thumbnail_path_in(cache, path, size);
    if target.is_file() {
        return Ok(target);
    }
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    // Written next to the target and renamed, so readers never see a half-written file.
    let partial = target.with_extension(format!("{}.part.png", std::process::id()));
    if FileKind::of(path) == FileKind::Video {
        video_frame(path, size, &partial)?;
    } else {
        let image = image::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let thumb = image.thumbnail(size, size);
        thumb.save_with_format(&partial, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&partial, &target).map_err(|e| e.to_string())?;
    Ok(target)
}

/// Cuts at most this large are made from the thumbnail of this size.
const SHAPED_FROM_THUMBNAIL: u32 = 512;

/// Where [`shaped`] caches its result; changes when the file changes.
pub fn shaped_path(path: &Path, width: u32, height: u32, radii: [f32; 4]) -> PathBuf {
    shaped_path_in(&cache_dir(), path, width, height, radii)
}

fn shaped_path_in(cache: &Path, path: &Path, width: u32, height: u32, radii: [f32; 4]) -> PathBuf {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let mut hasher = DefaultHasher::new();
    (path, modified, width, height, radii.map(f32::to_bits)).hash(&mut hasher);
    cache.join(format!("shaped-{:016x}.png", hasher.finish()))
}

/// The image at `path` cropped to fill `width` × `height` pixels, transparent outside a
/// rectangle whose corners (top left, top right, bottom right, bottom left) are rounded by
/// `radii` pixels, cached as PNG: a picture in a shape Slint's software renderer cannot clip
/// to. Small cuts of videos show a frame (see [`thumbnail`]). Blocking.
pub fn shaped(path: &Path, width: u32, height: u32, radii: [f32; 4]) -> Result<PathBuf, String> {
    shaped_in(&cache_dir(), path, width, height, radii)
}

fn shaped_in(
    cache: &Path,
    path: &Path,
    width: u32,
    height: u32,
    radii: [f32; 4],
) -> Result<PathBuf, String> {
    let target = shaped_path_in(cache, path, width, height, radii);
    if target.is_file() {
        return Ok(target);
    }
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    // Small cuts (grid tiles) come from the cached thumbnail: quicker, and videos have one.
    let source = if width.max(height) <= SHAPED_FROM_THUMBNAIL {
        thumbnail_in(cache, path, SHAPED_FROM_THUMBNAIL)?
    } else {
        path.to_path_buf()
    };
    let mut image = image::DynamicImage::ImageRgba8(decode(&source)?)
        .resize_to_fill(width.max(1), height.max(1), image::imageops::FilterType::Triangle)
        .into_rgba8();
    round_corners(&mut image, radii);
    let partial = target.with_extension(format!("{}.part.png", std::process::id()));
    image.save_with_format(&partial, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    std::fs::rename(&partial, &target).map_err(|e| e.to_string())?;
    Ok(target)
}

/// Fades the pixels outside the rounded corners (top left, top right, bottom right, bottom left)
/// out, with an anti-aliased edge.
pub fn round_corners(image: &mut image::RgbaImage, radii: [f32; 4]) {
    let (width, height) = (image.width() as f32, image.height() as f32);
    let limit = width.min(height) / 2.0;
    let [top_left, top_right, bottom_right, bottom_left] = radii.map(|r| r.clamp(0.0, limit));
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        // The corner this pixel is in, and the centre of its circle.
        let (radius, cx, cy) = match (px < width / 2.0, py < height / 2.0) {
            (true, true) => (top_left, top_left, top_left),
            (false, true) => (top_right, width - top_right, top_right),
            (false, false) => (bottom_right, width - bottom_right, height - bottom_right),
            (true, false) => (bottom_left, bottom_left, height - bottom_left),
        };
        let outside_x = if px < width / 2.0 { px < cx } else { px > cx };
        let outside_y = if py < height / 2.0 { py < cy } else { py > cy };
        if radius > 0.0 && outside_x && outside_y {
            let distance = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
            let coverage = (radius - distance + 0.5).clamp(0.0, 1.0);
            pixel.0[3] = (f32::from(pixel.0[3]) * coverage).round() as u8;
        }
    }
}

/// Writes a frame of a video, scaled to fit `size`, as PNG with ffmpeg.
fn video_frame(path: &Path, size: u32, target: &Path) -> Result<(), String> {
    let scale = format!("scale={size}:{size}:force_original_aspect_ratio=decrease");
    // One second in skips black intros; very short clips fall back to the first frame.
    for seek in [&["-ss", "1"][..], &[]] {
        let status = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y"])
            .args(seek)
            .arg("-i")
            .arg(path)
            .args(["-frames:v", "1", "-vf", &scale, "-f", "image2", "-c:v", "png"])
            .arg(target)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| format!("ffmpeg (needed for video thumbnails): {e}"))?;
        if status.success() && target.metadata().is_ok_and(|m| m.len() > 0) {
            return Ok(());
        }
    }
    let _ = std::fs::remove_file(target);
    Err(format!("{}: ffmpeg could not read a frame", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn lists_filtered_and_sorted() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["b.PNG", "a.jpg", "notes.txt", ".hidden.png"] {
            std::fs::write(dir.path().join(name), "").unwrap();
        }
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let names: Vec<_> = list(dir.path(), &["png", "jpg"]).into_iter().map(|e| e.name).collect();
        assert_eq!(names, vec!["sub", "a.jpg", "b.PNG"]);
    }

    #[test]
    fn thumbnails_are_cached() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("big.png");
        image::RgbImage::from_pixel(400, 200, image::Rgb([200, 10, 10])).save(&source).unwrap();
        let thumb = thumbnail_in(dir.path(), &source, 100).unwrap();
        let decoded = image::open(&thumb).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (100, 50));
        assert_eq!(thumbnail_in(dir.path(), &source, 100).unwrap(), thumb);
    }

    #[test]
    fn thumbnails_update_after_changes_within_the_same_second() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("image.png");
        let first_modified = UNIX_EPOCH + Duration::new(1_700_000_000, 100_000_000);
        let second_modified = UNIX_EPOCH + Duration::new(1_700_000_000, 900_000_000);

        image::RgbImage::from_pixel(4, 2, image::Rgb([255, 0, 0])).save(&source).unwrap();
        std::fs::File::open(&source).unwrap().set_modified(first_modified).unwrap();
        let first = thumbnail_in(dir.path(), &source, 2).unwrap();
        assert_eq!(image::open(&first).unwrap().to_rgb8().get_pixel(0, 0).0, [255, 0, 0]);

        image::RgbImage::from_pixel(4, 2, image::Rgb([0, 0, 255])).save(&source).unwrap();
        std::fs::File::open(&source).unwrap().set_modified(second_modified).unwrap();
        let second = thumbnail_in(dir.path(), &source, 2).unwrap();
        assert_ne!(first, second);
        assert_eq!(image::open(&second).unwrap().to_rgb8().get_pixel(0, 0).0, [0, 0, 255]);
        assert_eq!(thumbnail_in(dir.path(), &source, 2).unwrap(), second);
    }

    #[test]
    fn shapes_have_transparent_corners() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("wide.png");
        image::RgbImage::from_pixel(400, 200, image::Rgb([20, 200, 20])).save(&source).unwrap();
        let shaped = shaped_in(dir.path(), &source, 100, 100, [50.0, 0.0, 10.0, 0.0]).unwrap();
        let image = image::open(&shaped).unwrap().into_rgba8();
        assert_eq!((image.width(), image.height()), (100, 100));
        assert_eq!(image.get_pixel(0, 0).0[3], 0);
        assert_eq!(image.get_pixel(99, 0).0[3], 255);
        assert_eq!(image.get_pixel(99, 99).0[3], 0);
        assert_eq!(image.get_pixel(50, 50).0, [20, 200, 20, 255]);
    }

    #[test]
    fn expands_home() {
        assert_eq!(expand_home("~/Pictures"), xdg::home_dir().join("Pictures"));
        assert_eq!(expand_home("/abs"), PathBuf::from("/abs"));
    }
}
