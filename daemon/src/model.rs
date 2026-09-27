use crate::settings::SettingsSnapshot;

/// The MIME type every text entry is stored and restored as. Native Wayland
/// clients (GTK 4, Qt) ask for this; X11 names such as `UTF8_STRING` are only
/// understood by XWayland clients.
pub const TEXT_MIME: &str = "text/plain;charset=utf-8";

/// File lists are stored and restored as a URI list. The Nautilus
/// `x-special/gnome-copied-files` target is never restored, because a stale
/// `cut` marker makes a later paste *move* the files.
pub const URI_LIST_MIME: &str = "text/uri-list";

pub const GNOME_COPIED_FILES_MIME: &str = "x-special/gnome-copied-files";

pub const SENSITIVE_MIME: &str = "x-kde-passwordManagerHint";

/// MIME types the extension may restore. Anything else is refused on both
/// sides of the D-Bus boundary.
pub const RESTORABLE_MIMES: [&str; 4] = [TEXT_MIME, "image/png", "image/jpeg", URI_LIST_MIME];

const PREVIEW_CHARS: usize = 120;
const THUMB_SIZE: u32 = 96;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text = 0,
    Image = 1,
    Files = 2,
}

impl Kind {
    pub fn as_i64(self) -> i64 {
        self as i64
    }

    pub fn from_i64(value: i64) -> Option<Self> {
        match value {
            0 => Some(Kind::Text),
            1 => Some(Kind::Image),
            2 => Some(Kind::Files),
            _ => None,
        }
    }

    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn icon_name(self) -> &'static str {
        match self {
            Kind::Text => "format-text-symbolic",
            Kind::Image => "image-x-generic-symbolic",
            Kind::Files => "folder-symbolic",
        }
    }
}

/// A clipboard payload in its canonical stored form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Normalized {
    pub kind: Kind,
    pub mime: &'static str,
    pub data: Vec<u8>,
}

/// Converts an offered MIME type and payload into the canonical form that is
/// stored, deduplicated and restored. Returns `None` for unsupported types.
pub fn normalize(mime: &str, data: &[u8]) -> Option<Normalized> {
    let lowered = mime.trim().to_ascii_lowercase();
    match lowered.as_str() {
        "text/plain;charset=utf-8" | "text/plain" | "utf8_string" | "text" => Some(Normalized {
            kind: Kind::Text,
            mime: TEXT_MIME,
            data: strip_nuls(String::from_utf8_lossy(data).as_ref()).into_bytes(),
        }),
        // X11 STRING is ISO-8859-1: every byte maps to the code point of the
        // same value.
        "string" => Some(Normalized {
            kind: Kind::Text,
            mime: TEXT_MIME,
            data: strip_nuls(
                &data
                    .iter()
                    .map(|&byte| char::from(byte))
                    .collect::<String>(),
            )
            .into_bytes(),
        }),
        "image/png" => Some(Normalized {
            kind: Kind::Image,
            mime: "image/png",
            data: data.to_vec(),
        }),
        "image/jpeg" => Some(Normalized {
            kind: Kind::Image,
            mime: "image/jpeg",
            data: data.to_vec(),
        }),
        URI_LIST_MIME => uri_list(String::from_utf8_lossy(data).lines()),
        GNOME_COPIED_FILES_MIME => {
            let text = String::from_utf8_lossy(data);
            let mut lines = text.lines();
            // First line is the operation (`copy` or `cut`); it is dropped.
            match lines.next().map(str::trim) {
                Some("copy") | Some("cut") => uri_list(lines),
                _ => None,
            }
        }
        _ => None,
    }
}

fn strip_nuls(text: &str) -> String {
    text.trim_end_matches('\0').replace('\0', "")
}

fn uri_list<'a>(lines: impl Iterator<Item = &'a str>) -> Option<Normalized> {
    let uris: Vec<&str> = lines
        .map(|line| line.trim().trim_end_matches('\0'))
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    if uris.is_empty() {
        return None;
    }
    let mut data = uris.join("\r\n").into_bytes();
    data.extend_from_slice(b"\r\n");
    Some(Normalized {
        kind: Kind::Files,
        mime: URI_LIST_MIME,
        data,
    })
}

pub fn is_sensitive(mime: &str) -> bool {
    mime.trim().eq_ignore_ascii_case(SENSITIVE_MIME)
}

pub fn is_restorable(mime: &str) -> bool {
    RESTORABLE_MIMES.contains(&mime)
}

/// Content hash used for deduplication. Covers the MIME type so a PNG and a
/// JPEG of the same bytes (never happens in practice) stay distinct.
pub fn content_hash(mime: &str, data: &[u8]) -> String {
    let mut input = Vec::with_capacity(mime.len() + 1 + data.len());
    input.extend_from_slice(mime.as_bytes());
    input.push(0);
    input.extend_from_slice(data);
    glib::compute_checksum_for_data(glib::ChecksumType::Sha256, &input)
        .map(|digest| digest.to_string())
        .unwrap_or_default()
}

/// Text that search matches against: the text itself, or decoded file paths.
pub fn search_text(kind: Kind, data: &[u8]) -> Option<String> {
    match kind {
        Kind::Text => Some(String::from_utf8_lossy(data).into_owned()),
        Kind::Files => Some(file_names(data).join("\n")),
        Kind::Image => None,
    }
}

pub fn max_bytes_for(kind: Kind, settings: &SettingsSnapshot) -> usize {
    match kind {
        Kind::Text => settings.max_text_bytes,
        Kind::Image => settings.max_image_bytes,
        Kind::Files => 64 * 1024,
    }
}

pub fn preview(kind: Kind, data: &[u8]) -> String {
    match kind {
        Kind::Text => preview_text(&String::from_utf8_lossy(data)),
        Kind::Files => preview_files(data),
        Kind::Image => {
            let kib = data.len().div_ceil(1024);
            format!("{kib} KB image")
        }
    }
}

/// A small PNG thumbnail, generated once at capture time so the popup never
/// decodes full-size images.
pub fn thumbnail(kind: Kind, data: &[u8]) -> Option<Vec<u8>> {
    if kind != Kind::Image {
        return None;
    }
    let image = image::load_from_memory(data).ok()?;
    let thumb = image.thumbnail(THUMB_SIZE, THUMB_SIZE);
    let mut out = std::io::Cursor::new(Vec::new());
    thumb.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

fn preview_text(text: &str) -> String {
    let collapsed = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    truncate(&collapsed, PREVIEW_CHARS)
}

fn file_names(data: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(data)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|uri| match glib::filename_from_uri(uri) {
            Ok((path, _host)) => path.to_string_lossy().into_owned(),
            Err(_) => uri.to_string(),
        })
        .collect()
}

fn preview_files(data: &[u8]) -> String {
    let names = file_names(data);
    let Some(first) = names.first() else {
        return String::from("No files");
    };
    let first = basename(first);
    match names.len() - 1 {
        0 => first,
        extra => format!("{first} +{extra} more"),
    }
}

fn basename(path: &str) -> String {
    let trimmed = path.trim().trim_end_matches('/');
    trimmed
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(trimmed)
        .to_string()
}

fn truncate(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    if count <= limit {
        text.to_string()
    } else {
        let head: String = text.chars().take(limit.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

/// Row metadata. Never carries the payload, so listing history is cheap.
#[derive(Clone, Debug)]
pub struct EntryMeta {
    pub id: i64,
    pub kind: Kind,
    pub mime: String,
    pub preview: String,
    pub source: String,
    /// Milliseconds since the Unix epoch.
    pub ts: i64,
    pub pinned: bool,
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub thumb: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub meta: EntryMeta,
    pub data: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_all_text_types_to_utf8_plain() {
        for mime in [
            "text/plain;charset=utf-8",
            "TEXT/PLAIN",
            "UTF8_STRING",
            "TEXT",
        ] {
            let normalized = normalize(mime, b"hello").unwrap();
            assert_eq!(normalized.kind, Kind::Text);
            assert_eq!(normalized.mime, TEXT_MIME);
            assert_eq!(normalized.data, b"hello");
        }
    }

    #[test]
    fn decodes_x11_string_as_latin1() {
        let normalized = normalize("STRING", &[b'c', 0xe9]).unwrap();
        assert_eq!(normalized.data, "cé".as_bytes());
    }

    #[test]
    fn strips_nuls_from_text() {
        assert_eq!(normalize("text/plain", b"abc\0\0").unwrap().data, b"abc");
    }

    #[test]
    fn converts_gnome_copied_files_and_drops_cut_marker() {
        let normalized = normalize(
            GNOME_COPIED_FILES_MIME,
            b"cut\nfile:///tmp/a.txt\nfile:///tmp/b",
        )
        .unwrap();
        assert_eq!(normalized.kind, Kind::Files);
        assert_eq!(normalized.mime, URI_LIST_MIME);
        assert_eq!(normalized.data, b"file:///tmp/a.txt\r\nfile:///tmp/b\r\n");
        assert!(normalize(GNOME_COPIED_FILES_MIME, b"file:///tmp/a").is_none());
    }

    #[test]
    fn rejects_unsupported_and_sensitive_types() {
        assert!(normalize("text/html", b"<b>x</b>").is_none());
        assert!(normalize(SENSITIVE_MIME, b"secret").is_none());
        assert!(is_sensitive("x-kde-passwordManagerHint"));
        assert!(!is_sensitive("text/plain"));
    }

    #[test]
    fn only_canonical_types_are_restorable() {
        assert!(is_restorable(TEXT_MIME));
        assert!(is_restorable(URI_LIST_MIME));
        assert!(!is_restorable(GNOME_COPIED_FILES_MIME));
        assert!(!is_restorable("UTF8_STRING"));
    }

    #[test]
    fn hash_depends_on_mime_and_content() {
        let a = content_hash(TEXT_MIME, b"x");
        assert_eq!(a.len(), 64);
        assert_eq!(a, content_hash(TEXT_MIME, b"x"));
        assert_ne!(a, content_hash(TEXT_MIME, b"y"));
        assert_ne!(a, content_hash(URI_LIST_MIME, b"x"));
    }

    #[test]
    fn previews_text() {
        assert_eq!(preview(Kind::Text, b"first line\nsecond"), "first line");
        assert_eq!(preview(Kind::Text, b"  a   b  \n c "), "a b");
        assert_eq!(preview(Kind::Text, b"   \n  \n"), "");
    }

    #[test]
    fn previews_files_from_uri_list() {
        assert_eq!(
            preview(
                Kind::Files,
                b"file:///tmp/a%20b.txt\r\nfile:///home/u/c.txt\r\n"
            ),
            "a b.txt +1 more"
        );
        assert_eq!(preview(Kind::Files, b"file:///tmp/only.txt"), "only.txt");
        assert_eq!(preview(Kind::Files, b""), "No files");
    }

    #[test]
    fn previews_image_size() {
        assert_eq!(preview(Kind::Image, &[0; 2048]), "2 KB image");
        assert_eq!(preview(Kind::Image, &[0; 1]), "1 KB image");
    }

    #[test]
    fn search_text_uses_decoded_paths_for_files() {
        assert_eq!(
            search_text(Kind::Files, b"file:///home/u/needles/report.pdf\r\n"),
            Some("/home/u/needles/report.pdf".into())
        );
        assert_eq!(search_text(Kind::Image, b"x"), None);
    }

    #[test]
    fn thumbnails_png_images() {
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbImage::new(400, 200)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let thumb = thumbnail(Kind::Image, png.get_ref()).unwrap();
        let decoded = image::load_from_memory(&thumb).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (96, 48));
        assert!(thumbnail(Kind::Image, b"not an image").is_none());
        assert!(thumbnail(Kind::Text, b"text").is_none());
    }
}
