use crate::settings::SettingsSnapshot;

pub const TEXT_MIMES: [&str; 5] = [
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "STRING",
    "text/plain;charset=utf-8",
];

pub const IMAGE_MIMES: [&str; 2] = ["image/png", "image/jpeg"];

pub const FILES_MIME: &str = "x-special/gnome-copied-files";

pub const SENSITIVE_MIME: &str = "x-kde-passwordManagerHint";

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

    pub fn icon_name(self) -> &'static str {
        match self {
            Kind::Text => "format-text-symbolic",
            Kind::Image => "image-x-generic-symbolic",
            Kind::Files => "folder-symbolic",
        }
    }
}

pub fn classify(mime: &str) -> Option<Kind> {
    let mime = mime.trim().to_ascii_lowercase();
    if TEXT_MIMES.contains(&mime.as_str()) {
        Some(Kind::Text)
    } else if IMAGE_MIMES.contains(&mime.as_str()) {
        Some(Kind::Image)
    } else if mime == FILES_MIME {
        Some(Kind::Files)
    } else {
        None
    }
}

pub fn is_sensitive(mime: &str) -> bool {
    mime.trim().eq_ignore_ascii_case(SENSITIVE_MIME)
}

pub fn text_of(kind: Kind, data: &[u8]) -> Option<String> {
    match kind {
        Kind::Text | Kind::Files => Some(String::from_utf8_lossy(data).into_owned()),
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
        Kind::Files => preview_files(&String::from_utf8_lossy(data)),
        Kind::Image => {
            let kib = data.len().div_ceil(1024);
            format!("{kib} KB image")
        }
    }
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
    truncate(&collapsed, 120)
}

fn preview_files(data: &str) -> String {
    let mut lines = data.lines().filter(|line| !line.trim().is_empty());
    let first = match lines.next() {
        Some(line) if line.trim() == "copy" || line.trim() == "cut" => lines.next(),
        Some(line) => Some(line),
        None => None,
    };
    let first = match first {
        Some(path) => basename(path),
        None => return String::from("No files"),
    };
    let extra = lines.count();
    if extra == 0 {
        first
    } else {
        format!("{first} +{extra} more")
    }
}

fn basename(path: &str) -> String {
    let trimmed = path.trim();
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

#[derive(Clone, Debug)]
pub struct EntryMeta {
    pub id: i64,
    pub kind: Kind,
    pub mime: String,
    pub preview: String,
    pub source: String,
    pub ts: i64,
    pub pinned: bool,
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
    fn classifies_supported_mimes() {
        assert_eq!(classify("text/plain;charset=utf-8"), Some(Kind::Text));
        assert_eq!(classify("TEXT/PLAIN"), Some(Kind::Text));
        assert_eq!(classify("image/png"), Some(Kind::Image));
        assert_eq!(classify("x-special/gnome-copied-files"), Some(Kind::Files));
        assert_eq!(classify("text/html"), None);
        assert_eq!(classify(SENSITIVE_MIME), None);
    }

    #[test]
    fn detects_sensitive_mime() {
        assert!(is_sensitive("x-kde-passwordManagerHint"));
        assert!(!is_sensitive("text/plain"));
    }

    #[test]
    fn previews_text() {
        assert_eq!(preview(Kind::Text, b"first line\nsecond"), "first line");
        assert_eq!(preview(Kind::Text, b"  a   b  \n c "), "a b");
        assert_eq!(preview(Kind::Text, b"   \n  \n"), "");
    }

    #[test]
    fn previews_files() {
        assert_eq!(
            preview(Kind::Files, b"copy\n/tmp/a.txt\n/home/u/b.txt"),
            "a.txt +1 more"
        );
        assert_eq!(preview(Kind::Files, b"cut\n/tmp/only.txt"), "only.txt");
        assert_eq!(preview(Kind::Files, b""), "No files");
    }

    #[test]
    fn previews_image_size() {
        assert_eq!(preview(Kind::Image, &[0; 2048]), "2 KB image");
        assert_eq!(preview(Kind::Image, &[0; 1]), "1 KB image");
    }

    #[test]
    fn text_column_covers_text_and_files_only() {
        assert_eq!(text_of(Kind::Text, b"hello"), Some("hello".into()));
        assert_eq!(text_of(Kind::Files, b"copy\n/a"), Some("copy\n/a".into()));
        assert_eq!(text_of(Kind::Image, b"x"), None);
    }
}
