//! File-format classification from magic bytes, extension, and media type.
//!
//! # Why a retriever needs this
//!
//! A download manager sorts what it fetches, and the category decides real
//! behaviour: which directory the file lands in, and whether the extension can be
//! trusted. But it decides those things only if the classification is *right*, and
//! the three available signals disagree constantly on the open web.
//!
//! The precedence here is deliberate and is the opposite of what is convenient:
//!
//! 1. **Magic bytes win.** They describe the bytes that actually arrived.
//! 2. **Extension is a weak hint.** It is chosen by whoever named the file, is
//!    absent from most API URLs, and is trivially wrong.
//! 3. **`Content-Type` is the weakest signal of the three**, which surprises
//!    people. Servers routinely serve every archive as
//!    `application/octet-stream`, mislabel `.tar.gz` as `application/x-gzip` and
//!    vice versa, and — the case that matters — a captive portal or error page
//!    returns `text/html` with a 200 status, so a "download" completes and the
//!    saved file is a login page. Trusting the header there produces a file the
//!    user cannot open and cannot diagnose.
//!
//! When the signals conflict, [`Detection::conflict`] says so, and the CLI warns.
//! An HTML body delivered where an archive was expected is worth a warning even
//! though nothing failed: it is the signature of an interception, and the byte
//! count and status code both look fine.
//!
//! # Sniffing is not decompression
//!
//! Classification reads a prefix. It never decompresses, never rewrites, and never
//! renames without being asked. A retriever that silently unpacked its output
//! would be making a decision the caller did not delegate.

/// Broad category, in the sense a download manager sorts by.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    Video,
    Audio,
    Image,
    /// Archive or compressed stream.
    Archive,
    /// Document or e-book.
    Document,
    /// Executable, installer, or package.
    Application,
    /// Disk or filesystem image.
    DiskImage,
    Font,
    /// Structured data, source, or plain text.
    Data,
    /// Web page or markup — usually NOT what a download was meant to be.
    Markup,
    Unknown,
}

impl Category {
    /// Every variant, for exhaustive iteration.
    ///
    /// Kept next to the enum so adding a variant means updating this list in
    /// the same screenful — the compiler cannot enforce it, but the tests that
    /// iterate `ALL` will fail on a variant whose tables were forgotten.
    pub const ALL: [Category; 11] = [
        Category::Video,
        Category::Audio,
        Category::Image,
        Category::Archive,
        Category::Document,
        Category::Application,
        Category::DiskImage,
        Category::Font,
        Category::Data,
        Category::Markup,
        Category::Unknown,
    ];

    /// Conventional subdirectory name, matching what download managers use.
    pub fn directory(self) -> &'static str {
        match self {
            Category::Video => "Video",
            Category::Audio => "Music",
            Category::Image => "Images",
            Category::Archive => "Compressed",
            Category::Document => "Documents",
            Category::Application => "Programs",
            Category::DiskImage => "Images/Disk",
            Category::Font => "Fonts",
            Category::Data => "Data",
            Category::Markup => "Web",
            Category::Unknown => "Other",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Category::Video => "video",
            Category::Audio => "audio",
            Category::Image => "image",
            Category::Archive => "archive",
            Category::Document => "document",
            Category::Application => "application",
            Category::DiskImage => "disk image",
            Category::Font => "font",
            Category::Data => "data",
            Category::Markup => "markup",
            Category::Unknown => "unknown",
        }
    }
}

/// One recognised format.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Format {
    /// Short name, e.g. `"mp4"`.
    pub name: &'static str,
    pub category: Category,
    /// Canonical media type.
    pub media_type: &'static str,
    /// Usual extension, without the dot.
    pub extension: &'static str,
}

/// Where a classification came from, in descending trustworthiness.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Evidence {
    /// Magic bytes in the payload.
    Magic,
    /// The URL or filename extension.
    Extension,
    /// The server's `Content-Type`.
    MediaType,
    /// Nothing matched.
    None,
}

/// The result of classifying an object.
#[derive(Clone, Debug)]
pub struct Detection {
    pub format: Option<Format>,
    pub category: Category,
    pub evidence: Evidence,
    /// A human-readable disagreement between signals, when there is one.
    pub conflict: Option<String>,
}

impl Detection {
    /// True when the bytes are markup but the name or media type promised
    /// something else — the captive-portal and error-page signature.
    pub fn looks_intercepted(&self) -> bool {
        self.category == Category::Markup && self.conflict.is_some()
    }
}

const fn f(
    name: &'static str,
    category: Category,
    media_type: &'static str,
    extension: &'static str,
) -> Format {
    Format {
        name,
        category,
        media_type,
        extension,
    }
}

/// Magic-byte signature: bytes to match at an offset.
struct Sig {
    offset: usize,
    magic: &'static [u8],
    format: Format,
}

const fn sig(offset: usize, magic: &'static [u8], format: Format) -> Sig {
    Sig {
        offset,
        magic,
        format,
    }
}

/// Signatures, most specific first. Order matters: a Matroska file is a
/// specialisation of EBML, and OOXML/ODF/APK/JAR are all ZIP containers, so the
/// container check must come after any attempt to distinguish them.
static SIGS: &[Sig] = &[
    // ---- video ----------------------------------------------------------
    sig(4, b"ftypisom", f("mp4", Category::Video, "video/mp4", "mp4")),
    sig(4, b"ftypmp42", f("mp4", Category::Video, "video/mp4", "mp4")),
    sig(4, b"ftypM4V", f("m4v", Category::Video, "video/x-m4v", "m4v")),
    sig(4, b"ftypavc1", f("mp4", Category::Video, "video/mp4", "mp4")),
    sig(4, b"ftypdash", f("mp4", Category::Video, "video/mp4", "mp4")),
    sig(4, b"ftypqt", f("mov", Category::Video, "video/quicktime", "mov")),
    sig(0, b"\x1a\x45\xdf\xa3", f("matroska", Category::Video, "video/x-matroska", "mkv")),
    sig(0, b"FLV\x01", f("flv", Category::Video, "video/x-flv", "flv")),
    sig(0, b"\x00\x00\x01\xba", f("mpeg-ps", Category::Video, "video/mpeg", "mpg")),
    sig(0, b"\x00\x00\x01\xb3", f("mpeg-vid", Category::Video, "video/mpeg", "mpv")),
    sig(0, b"\x30\x26\xb2\x75", f("asf", Category::Video, "video/x-ms-asf", "wmv")),
    // ---- audio ----------------------------------------------------------
    sig(0, b"ID3", f("mp3", Category::Audio, "audio/mpeg", "mp3")),
    sig(0, b"\xff\xfb", f("mp3", Category::Audio, "audio/mpeg", "mp3")),
    sig(0, b"\xff\xf3", f("mp3", Category::Audio, "audio/mpeg", "mp3")),
    sig(0, b"\xff\xf2", f("mp3", Category::Audio, "audio/mpeg", "mp3")),
    sig(0, b"fLaC", f("flac", Category::Audio, "audio/flac", "flac")),
    sig(4, b"ftypM4A", f("m4a", Category::Audio, "audio/mp4", "m4a")),
    sig(0, b"OggS", f("ogg", Category::Audio, "audio/ogg", "ogg")),
    sig(0, b"\xff\xf1", f("aac", Category::Audio, "audio/aac", "aac")),
    sig(0, b"MThd", f("midi", Category::Audio, "audio/midi", "mid")),
    sig(0, b"#!AMR", f("amr", Category::Audio, "audio/amr", "amr")),
    // ---- image ----------------------------------------------------------
    sig(0, b"\x89PNG\r\n\x1a\n", f("png", Category::Image, "image/png", "png")),
    sig(0, b"\xff\xd8\xff", f("jpeg", Category::Image, "image/jpeg", "jpg")),
    sig(0, b"GIF89a", f("gif", Category::Image, "image/gif", "gif")),
    sig(0, b"GIF87a", f("gif", Category::Image, "image/gif", "gif")),
    sig(0, b"BM", f("bmp", Category::Image, "image/bmp", "bmp")),
    sig(0, b"II*\x00", f("tiff", Category::Image, "image/tiff", "tif")),
    sig(0, b"MM\x00*", f("tiff", Category::Image, "image/tiff", "tif")),
    sig(0, b"\x00\x00\x01\x00", f("ico", Category::Image, "image/x-icon", "ico")),
    // ---- fonts (before generic containers) ------------------------------
    sig(0, b"wOFF", f("woff", Category::Font, "font/woff", "woff")),
    sig(0, b"wOF2", f("woff2", Category::Font, "font/woff2", "woff2")),
    sig(0, b"\x00\x01\x00\x00\x00", f("truetype", Category::Font, "font/ttf", "ttf")),
    sig(0, b"OTTO", f("opentype", Category::Font, "font/otf", "otf")),
    sig(0, b"ttcf", f("ttc", Category::Font, "font/collection", "ttc")),
    // ---- archives and compressed streams -------------------------------
    sig(0, b"\x1f\x8b", f("gzip", Category::Archive, "application/gzip", "gz")),
    sig(0, b"BZh", f("bzip2", Category::Archive, "application/x-bzip2", "bz2")),
    sig(0, b"\xfd7zXZ\x00", f("xz", Category::Archive, "application/x-xz", "xz")),
    sig(0, b"\x28\xb5\x2f\xfd", f("zstd", Category::Archive, "application/zstd", "zst")),
    sig(0, b"\x04\x22\x4d\x18", f("lz4", Category::Archive, "application/x-lz4", "lz4")),
    sig(0, b"Rar!\x1a\x07", f("rar", Category::Archive, "application/vnd.rar", "rar")),
    sig(0, b"7z\xbc\xaf\x27\x1c", f("7z", Category::Archive, "application/x-7z-compressed", "7z")),
    sig(257, b"ustar", f("tar", Category::Archive, "application/x-tar", "tar")),
    sig(0, b"!<arch>", f("ar", Category::Archive, "application/x-archive", "a")),
    sig(0, b"\x5d\x00\x00", f("lzma", Category::Archive, "application/x-lzma", "lzma")),
    sig(0, b"\x1f\x9d", f("compress", Category::Archive, "application/x-compress", "Z")),
    // ---- documents ------------------------------------------------------
    sig(0, b"%PDF-", f("pdf", Category::Document, "application/pdf", "pdf")),
    sig(0, b"{\\rtf", f("rtf", Category::Document, "application/rtf", "rtf")),
    sig(0, b"\xd0\xcf\x11\xe0", f("ole2", Category::Document, "application/x-ole-storage", "doc")),
    sig(0, b"\x25\x21PS", f("postscript", Category::Document, "application/postscript", "ps")),
    sig(0, b"\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x42\x44\x53\x46", f("djvu-ish", Category::Document, "image/vnd.djvu", "djvu")),
    // ---- applications, installers, packages -----------------------------
    sig(0, b"MZ", f("pe", Category::Application, "application/vnd.microsoft.portable-executable", "exe")),
    sig(0, b"\x7fELF", f("elf", Category::Application, "application/x-executable", "")),
    sig(0, b"\xcf\xfa\xed\xfe", f("mach-o", Category::Application, "application/x-mach-binary", "")),
    sig(0, b"\xca\xfe\xba\xbe", f("mach-o-fat", Category::Application, "application/x-mach-binary", "")),
    sig(0, b"\xed\xab\xee\xdb", f("rpm", Category::Application, "application/x-rpm", "rpm")),
    sig(0, b"!<arch>\ndebian", f("deb", Category::Application, "application/vnd.debian.binary-package", "deb")),
    sig(0, b"\xde\xc0\x17\x0b", f("dmg-koly", Category::DiskImage, "application/x-apple-diskimage", "dmg")),
    // ---- disk images ----------------------------------------------------
    sig(32769, b"CD001", f("iso9660", Category::DiskImage, "application/x-iso9660-image", "iso")),
    sig(0, b"conectix", f("vhd", Category::DiskImage, "application/x-vhd", "vhd")),
    sig(0, b"QFI\xfb", f("qcow", Category::DiskImage, "application/x-qemu-disk", "qcow2")),
    sig(0, b"KDMV", f("vmdk", Category::DiskImage, "application/x-vmdk", "vmdk")),
    // ---- data and markup ------------------------------------------------
    sig(0, b"SQLite format 3\x00", f("sqlite", Category::Data, "application/vnd.sqlite3", "sqlite")),
    sig(0, b"PAR1", f("parquet", Category::Data, "application/vnd.apache.parquet", "parquet")),
    sig(0, b"\x93NUMPY", f("npy", Category::Data, "application/x-npy", "npy")),
    sig(0, b"\x89HDF\r\n\x1a\n", f("hdf5", Category::Data, "application/x-hdf5", "h5")),
    sig(0, b"<?xml", f("xml", Category::Data, "application/xml", "xml")),
    // ---- ZIP container LAST: OOXML, ODF, APK, JAR, EPUB all match it ----
    sig(0, b"PK\x03\x04", f("zip", Category::Archive, "application/zip", "zip")),
    sig(0, b"PK\x05\x06", f("zip-empty", Category::Archive, "application/zip", "zip")),
];

/// ZIP-container formats recognised by two independent signals: a member name
/// in the payload (ZIP_KINDS) and the filename extension (BY_EXT). Defined once
/// so the two tables cannot drift — a MIME type that differed between them
/// would classify the same file differently depending on which evidence won.
const APK: Format = f(
    "apk",
    Category::Application,
    "application/vnd.android.package-archive",
    "apk",
);
const DOCX: Format = f(
    "docx",
    Category::Document,
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "docx",
);
const XLSX: Format = f(
    "xlsx",
    Category::Document,
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "xlsx",
);
const PPTX: Format = f(
    "pptx",
    Category::Document,
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "pptx",
);
const EPUB: Format = f("epub", Category::Document, "application/epub+zip", "epub");
const JAR: Format = f(
    "jar",
    Category::Application,
    "application/java-archive",
    "jar",
);

/// ZIP-container specialisations, distinguished by a member name appearing in the
/// first few hundred bytes of the central-directory-adjacent prefix.
static ZIP_KINDS: &[(&[u8], Format)] = &[
    (b"AndroidManifest.xml", APK),
    (b"word/", DOCX),
    (b"xl/", XLSX),
    (b"ppt/", PPTX),
    (b"mimetypeapplication/epub", EPUB),
    (
        b"mimetypeapplication/vnd.oasis.opendocument.text",
        f(
            "odt",
            Category::Document,
            "application/vnd.oasis.opendocument.text",
            "odt",
        ),
    ),
    (b"META-INF/MANIFEST.MF", JAR),
];

/// Extension table, used when the payload is unavailable or unrecognised.
static BY_EXT: &[(&str, Format)] = &[
    ("mp4", f("mp4", Category::Video, "video/mp4", "mp4")),
    (
        "mkv",
        f("matroska", Category::Video, "video/x-matroska", "mkv"),
    ),
    ("avi", f("avi", Category::Video, "video/x-msvideo", "avi")),
    ("webm", f("webm", Category::Video, "video/webm", "webm")),
    ("mov", f("mov", Category::Video, "video/quicktime", "mov")),
    ("flv", f("flv", Category::Video, "video/x-flv", "flv")),
    ("wmv", f("asf", Category::Video, "video/x-ms-asf", "wmv")),
    ("m4v", f("m4v", Category::Video, "video/x-m4v", "m4v")),
    ("ts", f("mpeg-ts", Category::Video, "video/mp2t", "ts")),
    ("mp3", f("mp3", Category::Audio, "audio/mpeg", "mp3")),
    ("flac", f("flac", Category::Audio, "audio/flac", "flac")),
    ("wav", f("wav", Category::Audio, "audio/wav", "wav")),
    ("aac", f("aac", Category::Audio, "audio/aac", "aac")),
    ("ogg", f("ogg", Category::Audio, "audio/ogg", "ogg")),
    ("opus", f("opus", Category::Audio, "audio/opus", "opus")),
    ("m4a", f("m4a", Category::Audio, "audio/mp4", "m4a")),
    ("wma", f("wma", Category::Audio, "audio/x-ms-wma", "wma")),
    ("mid", f("midi", Category::Audio, "audio/midi", "mid")),
    ("png", f("png", Category::Image, "image/png", "png")),
    ("jpg", f("jpeg", Category::Image, "image/jpeg", "jpg")),
    ("jpeg", f("jpeg", Category::Image, "image/jpeg", "jpg")),
    ("gif", f("gif", Category::Image, "image/gif", "gif")),
    ("webp", f("webp", Category::Image, "image/webp", "webp")),
    ("avif", f("avif", Category::Image, "image/avif", "avif")),
    ("heic", f("heic", Category::Image, "image/heic", "heic")),
    ("svg", f("svg", Category::Image, "image/svg+xml", "svg")),
    ("tif", f("tiff", Category::Image, "image/tiff", "tif")),
    ("tiff", f("tiff", Category::Image, "image/tiff", "tif")),
    ("zip", f("zip", Category::Archive, "application/zip", "zip")),
    ("gz", f("gzip", Category::Archive, "application/gzip", "gz")),
    (
        "tgz",
        f("tar.gz", Category::Archive, "application/gzip", "tgz"),
    ),
    (
        "bz2",
        f("bzip2", Category::Archive, "application/x-bzip2", "bz2"),
    ),
    ("xz", f("xz", Category::Archive, "application/x-xz", "xz")),
    (
        "zst",
        f("zstd", Category::Archive, "application/zstd", "zst"),
    ),
    (
        "rar",
        f("rar", Category::Archive, "application/vnd.rar", "rar"),
    ),
    (
        "7z",
        f("7z", Category::Archive, "application/x-7z-compressed", "7z"),
    ),
    (
        "tar",
        f("tar", Category::Archive, "application/x-tar", "tar"),
    ),
    (
        "lz4",
        f("lz4", Category::Archive, "application/x-lz4", "lz4"),
    ),
    (
        "pdf",
        f("pdf", Category::Document, "application/pdf", "pdf"),
    ),
    ("epub", EPUB),
    ("docx", DOCX),
    ("xlsx", XLSX),
    ("pptx", PPTX),
    (
        "doc",
        f("ole2", Category::Document, "application/msword", "doc"),
    ),
    (
        "rtf",
        f("rtf", Category::Document, "application/rtf", "rtf"),
    ),
    (
        "djvu",
        f("djvu", Category::Document, "image/vnd.djvu", "djvu"),
    ),
    (
        "exe",
        f(
            "pe",
            Category::Application,
            "application/vnd.microsoft.portable-executable",
            "exe",
        ),
    ),
    (
        "msi",
        f("msi", Category::Application, "application/x-msi", "msi"),
    ),
    (
        "dmg",
        f(
            "dmg",
            Category::DiskImage,
            "application/x-apple-diskimage",
            "dmg",
        ),
    ),
    (
        "pkg",
        f(
            "pkg",
            Category::Application,
            "application/x-newton-compatible-pkg",
            "pkg",
        ),
    ),
    (
        "deb",
        f(
            "deb",
            Category::Application,
            "application/vnd.debian.binary-package",
            "deb",
        ),
    ),
    (
        "rpm",
        f("rpm", Category::Application, "application/x-rpm", "rpm"),
    ),
    ("apk", APK),
    (
        "appimage",
        f(
            "appimage",
            Category::Application,
            "application/x-executable",
            "AppImage",
        ),
    ),
    ("jar", JAR),
    (
        "whl",
        f("wheel", Category::Application, "application/zip", "whl"),
    ),
    (
        "iso",
        f(
            "iso9660",
            Category::DiskImage,
            "application/x-iso9660-image",
            "iso",
        ),
    ),
    (
        "img",
        f(
            "raw-image",
            Category::DiskImage,
            "application/octet-stream",
            "img",
        ),
    ),
    (
        "qcow2",
        f(
            "qcow",
            Category::DiskImage,
            "application/x-qemu-disk",
            "qcow2",
        ),
    ),
    (
        "vmdk",
        f("vmdk", Category::DiskImage, "application/x-vmdk", "vmdk"),
    ),
    (
        "vhd",
        f("vhd", Category::DiskImage, "application/x-vhd", "vhd"),
    ),
    ("ttf", f("truetype", Category::Font, "font/ttf", "ttf")),
    ("otf", f("opentype", Category::Font, "font/otf", "otf")),
    ("woff", f("woff", Category::Font, "font/woff", "woff")),
    ("woff2", f("woff2", Category::Font, "font/woff2", "woff2")),
    (
        "json",
        f("json", Category::Data, "application/json", "json"),
    ),
    ("csv", f("csv", Category::Data, "text/csv", "csv")),
    ("xml", f("xml", Category::Data, "application/xml", "xml")),
    ("txt", f("text", Category::Data, "text/plain", "txt")),
    (
        "parquet",
        f(
            "parquet",
            Category::Data,
            "application/vnd.apache.parquet",
            "parquet",
        ),
    ),
    (
        "sqlite",
        f(
            "sqlite",
            Category::Data,
            "application/vnd.sqlite3",
            "sqlite",
        ),
    ),
    ("h5", f("hdf5", Category::Data, "application/x-hdf5", "h5")),
    ("npy", f("npy", Category::Data, "application/x-npy", "npy")),
    ("html", f("html", Category::Markup, "text/html", "html")),
    ("htm", f("html", Category::Markup, "text/html", "html")),
];

/// Classify by magic bytes alone.
pub fn from_magic(buf: &[u8]) -> Option<Format> {
    if starts_html(buf) {
        return Some(f("html", Category::Markup, "text/html", "html"));
    }
    // RIFF containers carry their kind at offset 8.
    if buf.len() >= 12 && &buf[0..4] == b"RIFF" {
        return match &buf[8..12] {
            b"WAVE" => Some(f("wav", Category::Audio, "audio/wav", "wav")),
            b"AVI " => Some(f("avi", Category::Video, "video/x-msvideo", "avi")),
            b"WEBP" => Some(f("webp", Category::Image, "image/webp", "webp")),
            _ => None,
        };
    }
    // ISO-BMFF brands live at offset 4 after a size field; `ftyp` then a brand.
    if buf.len() >= 12 && &buf[4..8] == b"ftyp" {
        let brand = &buf[8..12];
        let hit = match brand {
            b"avif" | b"avis" => Some(f("avif", Category::Image, "image/avif", "avif")),
            b"heic" | b"heix" | b"hevc" => Some(f("heic", Category::Image, "image/heic", "heic")),
            b"M4A " => Some(f("m4a", Category::Audio, "audio/mp4", "m4a")),
            b"M4V " => Some(f("m4v", Category::Video, "video/x-m4v", "m4v")),
            _ => None,
        };
        if hit.is_some() {
            return hit;
        }
    }
    for s in SIGS {
        let end = s.offset + s.magic.len();
        if buf.len() >= end && &buf[s.offset..end] == s.magic {
            // A ZIP container may be something more specific.
            if s.format.name.starts_with("zip") {
                if let Some(k) = zip_kind(buf) {
                    return Some(k);
                }
            }
            return Some(s.format);
        }
    }
    None
}

fn starts_html(buf: &[u8]) -> bool {
    let mut head = buf.trim_ascii_start();
    head = head
        .strip_prefix(b"\xef\xbb\xbf")
        .unwrap_or(head)
        .trim_ascii_start();
    loop {
        let end_marker: &[u8] = if head.starts_with(b"<!--") {
            b"-->"
        } else if head.starts_with(b"<?xml ") {
            b"?>"
        } else {
            break;
        };
        let Some(end) = head.windows(end_marker.len()).position(|w| w == end_marker) else {
            return false;
        };
        head = head[end + end_marker.len()..].trim_ascii_start();
    }
    [b"<html".as_slice(), b"<!doctype html".as_slice()]
        .iter()
        .any(|tag| {
            head.get(..tag.len())
                .is_some_and(|s| s.eq_ignore_ascii_case(tag))
                && head
                    .get(tag.len())
                    .is_some_and(|b| b.is_ascii_whitespace() || matches!(b, b'>' | b'/'))
        })
}

fn zip_kind(buf: &[u8]) -> Option<Format> {
    let window = &buf[..buf.len().min(4096)];
    for (needle, fmt) in ZIP_KINDS {
        if window
            .windows(needle.len())
            .any(|w| w.eq_ignore_ascii_case(needle))
        {
            return Some(*fmt);
        }
    }
    None
}

/// Classify by filename or URL path extension.
pub fn from_extension(name: &str) -> Option<Format> {
    let base = name.split(['?', '#']).next().unwrap_or(name);
    let lower = base.to_ascii_lowercase();
    // Compound extensions first: `.tar.gz` is a tar, not merely a gzip, and
    // sorting it as an archive is right either way but the name should be exact.
    for (suffix, fmt) in [
        (
            ".tar.gz",
            f("tar.gz", Category::Archive, "application/gzip", "tar.gz"),
        ),
        (
            ".tar.bz2",
            f(
                "tar.bz2",
                Category::Archive,
                "application/x-bzip2",
                "tar.bz2",
            ),
        ),
        (
            ".tar.xz",
            f("tar.xz", Category::Archive, "application/x-xz", "tar.xz"),
        ),
        (
            ".tar.zst",
            f("tar.zst", Category::Archive, "application/zstd", "tar.zst"),
        ),
    ] {
        if lower.ends_with(suffix) {
            return Some(fmt);
        }
    }
    let ext = lower.rsplit_once('.')?.1;
    BY_EXT.iter().find(|(e, _)| *e == ext).map(|(_, fmt)| *fmt)
}

/// Classify by a `Content-Type` header value.
pub fn from_media_type(ct: &str) -> Option<Format> {
    let base = ct.split(';').next()?.trim().to_ascii_lowercase();
    if base == "application/xhtml+xml" {
        return Some(f("html", Category::Markup, "application/xhtml+xml", "html"));
    }
    if base.is_empty() || base == "application/octet-stream" {
        // The universal "I don't know" of HTTP. Treating it as a classification
        // would overwrite better evidence with none.
        return None;
    }
    if let Some(hit) = BY_EXT.iter().find(|(_, f)| f.media_type == base) {
        return Some(hit.1);
    }
    // Fall back to the type's top-level category.
    let cat = match base.split('/').next()? {
        "video" => Category::Video,
        "audio" => Category::Audio,
        "image" => Category::Image,
        "font" => Category::Font,
        "text" if base == "text/html" => Category::Markup,
        "text" => Category::Data,
        _ => return None,
    };
    Some(Format {
        name: "generic",
        category: cat,
        media_type: "",
        extension: "",
    })
}

/// Combine all three signals, with magic bytes taking precedence.
///
/// `prefix` may be empty (nothing fetched yet); `name` is the filename or URL;
/// `content_type` is the server's header if it sent one.
pub fn detect_format(prefix: &[u8], name: &str, content_type: Option<&str>) -> Detection {
    let magic = from_magic(prefix);
    let ext = from_extension(name);
    let mt = content_type.and_then(from_media_type);

    let (format, evidence) = match (magic, ext, mt) {
        (Some(m), _, _) => (Some(m), Evidence::Magic),
        (None, Some(e), _) => (Some(e), Evidence::Extension),
        (None, None, Some(t)) => (Some(t), Evidence::MediaType),
        (None, None, None) => (None, Evidence::None),
    };
    let category = format.map(|f| f.category).unwrap_or(Category::Unknown);

    // Conflicts are reported, not resolved silently. The one that matters is
    // markup arriving where a real file was expected.
    let mut conflict = None;
    if let (Some(m), Some(e)) = (magic, ext) {
        if m.category != e.category {
            conflict = Some(format!(
                "content is {} ({}) but the name says {} ({})",
                m.name,
                m.category.as_str(),
                e.name,
                e.category.as_str()
            ));
        }
    }
    if conflict.is_none() {
        if let (Some(m), Some(t)) = (magic, mt) {
            if m.category != t.category {
                conflict = Some(format!(
                    "content is {} ({}) but the server said {} ({})",
                    m.name,
                    m.category.as_str(),
                    content_type.unwrap_or("?"),
                    t.category.as_str()
                ));
            }
        }
    }
    Detection {
        format,
        category,
        evidence,
        conflict,
    }
}

// ---------------------------------------------------------------------------
// Human-readable descriptions
// ---------------------------------------------------------------------------

/// One-line label and a short explanation, keyed by format name.
///
/// Keyed by NAME rather than carried as fields on `Format` on purpose: `mp4`
/// appears in five table entries (four magic brands plus the extension), so a
/// per-entry field would mean five copies of the same prose to keep in step. One
/// row per format is the single source of truth, and a test asserts every format
/// reachable from either table has one.
///
/// The text is aimed at a user deciding what to do with a file they just fetched,
/// so it says what the thing is FOR and what will bite them — that a `.gz` holds
/// exactly one stream, that re-saving a JPEG degrades it, that an HTML body where
/// an archive was expected usually means a login wall — rather than restating the
/// name in longer words.
static DESCRIPTIONS: &[(&str, &str, &str)] = &[
    ("7z", "7-Zip archive", "Open archive, usually LZMA2. Better ratios than ZIP; supports strong encryption."),
    ("aac", "AAC audio", "Lossy audio, better quality than MP3 at the same bitrate. Standard for streaming."),
    ("amr", "AMR speech audio", "Narrowband speech codec from mobile telephony. Poor for music, small for voice."),
    ("apk", "Android package", "Android application. A ZIP with a manifest and compiled Dalvik bytecode."),
    ("appimage", "AppImage application", "Self-contained Linux application: mark it executable and run it, no installation."),
    ("ar", "ar archive", "Unix archive format. Holds static libraries, and is the outer wrapper of a .deb."),
    ("asf", "Windows Media video", "Microsoft ASF container, usually WMV video. Needs a codec pack outside Windows."),
    ("avi", "AVI video", "Microsoft's 1992 container. Widely readable but cannot carry modern features like proper subtitles."),
    ("avif", "AVIF image", "AV1-based still image. Substantially smaller than JPEG at equal quality; newer decoder support."),
    ("bmp", "Bitmap image", "Uncompressed Windows bitmap. Very large for its content."),
    ("bzip2", "bzip2 stream", "Slower than gzip, compresses somewhat better. Largely displaced by xz and zstd."),
    ("compress", "compress (.Z) stream", "Unix compress from the 1980s. Obsolete; kept for old archives."),
    ("csv", "CSV table", "Delimited plain-text table. No types and no schema, so column meaning is a convention."),
    ("deb", "Debian package", "Package for Debian, Ubuntu, and derivatives. Installed with apt or dpkg."),
    ("djvu", "DjVu document", "Scanned-document format aimed at small sizes for text-heavy page images."),
    ("dmg", "macOS disk image", "Apple disk image. Double-click to mount, then drag the application out; do not run it from inside the mounted image."),
    ("djvu-ish", "DjVu-like document", "Scanned-document container matching a DjVu-family signature."),
    ("dmg-koly", "macOS disk image", "Apple disk image. Double-click to mount, then drag the application out."),
    ("docx", "Word document", "OOXML word processor file. A ZIP of XML parts."),
    ("elf", "ELF executable", "Linux, BSD, or Unix binary or shared library. Architecture-specific."),
    ("epub", "EPUB e-book", "Reflowable e-book (a ZIP of XHTML), so text adapts to the screen."),
    ("flac", "FLAC audio", "Lossless compression, typically 50-60% of WAV size with no quality loss."),
    ("flv", "Flash video", "Legacy container from the Flash era. Still produced by some streaming tools."),
    ("generic", "Unclassified", "Recognised only by its media type; the specific format is unknown."),
    ("gif", "GIF image", "256 colours, supports simple animation. Superseded by PNG for stills and video for animation."),
    ("gzip", "gzip stream", "Compresses a SINGLE stream, so a .gz almost always wraps a .tar to hold more than one file."),
    ("hdf5", "HDF5 dataset", "Hierarchical container for large scientific arrays, with internal compression."),
    ("heic", "HEIC image", "HEIF/HEVC still image. What iPhones shoot by default; limited support outside Apple."),
    ("html", "HTML page", "A web page. Where a real file was expected, this usually means a login wall, a captive portal, or an error page saved with a success status."),
    ("ico", "Windows icon", "Container holding several small sizes of the same icon."),
    ("iso9660", "ISO disk image", "Optical-disc image. Mount it, or write it to a USB stick to install an operating system."),
    ("jar", "Java archive", "A ZIP of Java classes, run with java -jar."),
    ("jpeg", "JPEG image", "Lossy photographic image. Re-saving degrades it each time; no transparency."),
    ("json", "JSON data", "Structured text data. Human-readable, and the usual format for web APIs."),
    ("lz4", "LZ4 stream", "Optimised for speed over ratio. Used where decompression time matters more than size."),
    ("lzma", "LZMA stream", "The algorithm behind xz and 7z, in its bare stream form."),
    ("m4a", "AAC audio (MP4)", "AAC in an MP4 container. What iTunes and most phones produce."),
    ("m4v", "MPEG-4 video (Apple)", "MP4 with an Apple-specific brand; plays anywhere MP4 does."),
    ("mach-o", "macOS executable", "Mach-O binary built for a single architecture, so it runs on either Apple silicon or Intel but not both."),
    ("mach-o-fat", "macOS universal binary", "Mach-O holding several architectures (for example arm64 and x86_64) in one file."),
    ("matroska", "Matroska video", "Open container that can hold almost any codec, plus multiple subtitle and audio tracks."),
    ("midi", "MIDI sequence", "Not audio: performance instructions. What it sounds like depends on the synthesiser."),
    ("mov", "QuickTime movie", "Apple's container. Often used for camera and editing masters, so files are large."),
    ("mp3", "MP3 audio", "Lossy audio, universally playable. Quality depends on the bitrate it was encoded at."),
    ("mp4", "MPEG-4 video", "The common web and device video container. Almost always H.264 or H.265 video with AAC audio."),
    ("mpeg-ps", "MPEG program stream", "DVD-era container. Robust to truncation, which is why broadcast uses its transport-stream sibling."),
    ("mpeg-ts", "MPEG transport stream", "Broadcast and HLS segment format. Designed to be joined and cut at any point."),
    ("mpeg-vid", "MPEG elementary video", "Raw MPEG video with no container, so no audio and no timing metadata."),
    ("msi", "Windows installer", "Windows Installer package, driven by msiexec."),
    ("npy", "NumPy array", "A single NumPy array with its dtype and shape."),
    ("odt", "OpenDocument text", "ODF word processor file, the ISO-standard alternative to .docx."),
    ("ogg", "Ogg audio", "Open container, usually Vorbis or Opus. Royalty-free alternative to MP3/AAC."),
    ("ole2", "Legacy Office document", "Pre-2007 Office binary (.doc/.xls/.ppt) or another OLE2 compound file."),
    ("opentype", "OpenType font", "Outline font with advanced typography (ligatures, alternates, variable axes)."),
    ("opus", "Opus audio", "Modern lossy codec, best-in-class at low bitrates. Used for voice and streaming."),
    ("parquet", "Parquet dataset", "Columnar analytics format: compressed, typed, and fast to query by column."),
    ("pdf", "PDF document", "Fixed-layout document that renders identically everywhere. May be text or scanned images."),
    ("pe", "Windows executable", "Windows PE binary (.exe/.dll). Runs on Windows only."),
    ("pkg", "macOS installer package", "macOS installer, opened by Installer.app."),
    ("png", "PNG image", "Lossless, with transparency. Right for screenshots, diagrams, and line art."),
    ("postscript", "PostScript document", "Page-description program for printers. PDF's predecessor."),
    ("pptx", "PowerPoint presentation", "OOXML presentation, internally a ZIP of XML parts, so it opens outside PowerPoint too."),
    ("qcow", "QEMU disk image", "QEMU/KVM virtual disk with copy-on-write and sparse allocation."),
    ("rar", "RAR archive", "Proprietary archive with strong recovery-record and multi-volume support. Extraction needs unrar."),
    ("raw-image", "Raw disk image", "Byte-for-byte copy of a disk or partition. Write with care: it overwrites a whole device."),
    ("rpm", "RPM package", "Package for Fedora, RHEL, SUSE, and derivatives. Installed with dnf or rpm."),
    ("rtf", "Rich Text Format", "Portable formatted text. Readable by nearly every word processor."),
    ("sqlite", "SQLite database", "A complete relational database in one file."),
    ("svg", "SVG vector image", "XML vector graphics: scales to any size without loss. Text, not pixels."),
    ("tar", "tar archive", "Uncompressed container that preserves permissions, ownership, and symlinks. Usually paired with a compressor."),
    ("tar.gz", "gzip-compressed tar", "The standard Unix source and release bundle: tar for structure, gzip for size."),
    ("tar.xz", "xz-compressed tar", "tar with xz, for a smaller download at the cost of slower extraction."),
    ("tar.zst", "Zstandard-compressed tar", "tar with zstd: near-xz size, far faster to extract. Arch Linux packages use it."),
    ("text", "Plain text", "Unstructured text with no formatting and no declared encoding, so the character set is a guess."),
    ("tiff", "TIFF image", "Flexible, often lossless. Standard for scanning, printing, and geospatial rasters."),
    ("truetype", "TrueType font", "Outline font, installable on every mainstream operating system."),
    ("ttc", "TrueType collection", "Several related fonts sharing outlines in one file."),
    ("vhd", "Hyper-V disk image", "Microsoft virtual hard disk, attachable by Hyper-V and by Windows Disk Management."),
    ("vmdk", "VMware disk image", "VMware virtual disk, also readable by VirtualBox and by qemu-img for conversion."),
    ("wav", "WAV audio", "Uncompressed PCM. Large but exact; the usual interchange format for editing."),
    ("webm", "WebM video", "Matroska restricted to royalty-free codecs (VP8/VP9/AV1 with Vorbis/Opus). What browsers play natively."),
    ("webp", "WebP image", "Google's format, lossy or lossless, with transparency. Smaller than JPEG/PNG at similar quality."),
    ("wheel", "Python wheel", "Built Python package (a ZIP), installed with pip."),
    ("wma", "Windows Media audio", "Microsoft's lossy codec. Playable outside Windows only with extra codecs."),
    ("woff", "WOFF web font", "Compressed font for the web, loaded by CSS @font-face."),
    ("woff2", "WOFF2 web font", "Brotli-compressed web font, roughly 30% smaller than WOFF."),
    ("xlsx", "Excel spreadsheet", "OOXML spreadsheet, internally a ZIP of XML parts; formulas are stored, not only their results."),
    ("xml", "XML document", "Structured markup. Could be data, a configuration file, or a document."),
    ("xz", "xz stream", "High compression ratio, slow to compress and memory-hungry to decompress."),
    ("zip", "ZIP archive", "The general-purpose archive. Members are compressed individually, so one can be extracted without the rest."),
    ("zip-empty", "Empty ZIP archive", "A structurally valid ZIP containing no members at all, which usually signals a failed build."),
    ("zstd", "Zstandard stream", "Modern compressor: near-xz ratios at gzip-like speed. Increasingly the default."),
];

/// Every known format, for building a help screen, a GUI tooltip table, or a
/// file-type filter.
///
/// Exposed as data rather than as printed text so a GUI can render it however it
/// likes and a CLI can dump it as JSON. Deduplicated by name, since `mp4` and
/// friends appear in several signature entries.
pub fn catalogue() -> Vec<(
    &'static str,
    Category,
    &'static str,
    &'static str,
    &'static str,
)> {
    let mut out: Vec<(&str, Category, &str, &str, &str)> = Vec::new();
    let mut push = |f: &Format| {
        if out.iter().any(|(n, ..)| *n == f.name) {
            return;
        }
        let (label, text) = describe(f.name).unwrap_or(("Unknown format", ""));
        out.push((f.name, f.category, f.extension, label, text));
    };
    for s in SIGS {
        push(&s.format);
    }
    for (_, f) in ZIP_KINDS {
        push(f);
    }
    for (_, f) in BY_EXT {
        push(f);
    }
    out.sort_by(|a, b| (a.1.as_str(), a.0).cmp(&(b.1.as_str(), b.0)));
    out
}

/// Extensions this build recognises, for a GUI open/save filter.
pub fn known_extensions() -> Vec<&'static str> {
    let mut v: Vec<&str> = BY_EXT.iter().map(|(e, _)| *e).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Label and explanation for a format name.
pub fn describe(name: &str) -> Option<(&'static str, &'static str)> {
    DESCRIPTIONS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, label, text)| (*label, *text))
}

impl Format {
    /// Short human label, e.g. "gzip-compressed tar".
    pub fn label(&self) -> &'static str {
        describe(self.name)
            .map(|(l, _)| l)
            .unwrap_or("Unknown format")
    }

    /// One-sentence explanation of what the format is for.
    pub fn description(&self) -> &'static str {
        describe(self.name)
            .map(|(_, d)| d)
            .unwrap_or("No description available for this format.")
    }

    /// `label — description`, for a tooltip or a CLI hint line.
    pub fn hint(&self) -> String {
        format!("{} — {}", self.label(), self.description())
    }
}

impl Category {
    /// What this category is, for a GUI group header or a CLI legend.
    pub fn description(self) -> &'static str {
        match self {
            Category::Video => "Moving pictures with sound. Container and codec are separate choices, so a file that will not play usually needs a codec rather than a different container.",
            Category::Audio => "Sound only. Lossy formats discard detail permanently; lossless ones do not.",
            Category::Image => "Still pictures. Lossy formats degrade on every re-save; lossless and vector ones do not.",
            Category::Archive => "One or more files packed together, usually compressed. Extract before use.",
            Category::Document => "Formatted text for reading or printing, either fixed-layout or reflowable.",
            Category::Application => "Executable software or an installable package. Platform-specific, and worth verifying before running.",
            Category::DiskImage => "A whole filesystem or disc in one file. Mount it rather than extracting it.",
            Category::Font => "Typefaces for installing on a system or loading in a web page.",
            Category::Data => "Structured or plain data for programs to read rather than for direct viewing.",
            Category::Markup => "A web page. Where a real file was expected, this usually means a login wall, a captive portal, or an error page served with a success status.",
            Category::Unknown => "Not recognised from its content, name, or media type.",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad(head: &[u8], n: usize) -> Vec<u8> {
        let mut v = head.to_vec();
        v.resize(n.max(head.len()), 0);
        v
    }

    #[test]
    fn every_magic_signature_is_recognised() {
        // A signature table nobody exercises is a table that silently rots.
        let cases: &[(&[u8], &str, Category)] = &[
            (b"\x89PNG\r\n\x1a\n", "png", Category::Image),
            (b"\xff\xd8\xff\xe0", "jpeg", Category::Image),
            (b"GIF89a", "gif", Category::Image),
            (b"%PDF-1.7", "pdf", Category::Document),
            (b"\x1f\x8b\x08\x00", "gzip", Category::Archive),
            (b"BZh91AY", "bzip2", Category::Archive),
            (b"\xfd7zXZ\x00\x00", "xz", Category::Archive),
            (b"\x28\xb5\x2f\xfd\x00", "zstd", Category::Archive),
            (b"Rar!\x1a\x07\x00", "rar", Category::Archive),
            (b"7z\xbc\xaf\x27\x1c", "7z", Category::Archive),
            (b"ID3\x03\x00", "mp3", Category::Audio),
            (b"fLaC\x00\x00", "flac", Category::Audio),
            (b"OggS\x00\x02", "ogg", Category::Audio),
            (b"\x1a\x45\xdf\xa3\x01", "matroska", Category::Video),
            (b"FLV\x01\x05", "flv", Category::Video),
            (b"MZ\x90\x00", "pe", Category::Application),
            (b"\x7fELF\x02\x01", "elf", Category::Application),
            (b"\xed\xab\xee\xdb", "rpm", Category::Application),
            (b"wOFF\x00\x01", "woff", Category::Font),
            (b"wOF2\x00\x01", "woff2", Category::Font),
            (b"OTTO\x00\x01", "opentype", Category::Font),
            (b"SQLite format 3\x00", "sqlite", Category::Data),
            (b"PAR1", "parquet", Category::Data),
            (b"\x89HDF\r\n\x1a\n", "hdf5", Category::Data),
            (b"\x93NUMPY\x01", "npy", Category::Data),
            (b"<!DOCTYPE html><html>", "html", Category::Markup),
            (b"<?xml version=\"1.0\"?>", "xml", Category::Data),
            (b"\xd0\xcf\x11\xe0\xa1\xb1", "ole2", Category::Document),
            (b"{\\rtf1\\ansi", "rtf", Category::Document),
            (b"QFI\xfb\x00", "qcow", Category::DiskImage),
            (b"KDMV\x01", "vmdk", Category::DiskImage),
            (b"conectix\x00", "vhd", Category::DiskImage),
        ];
        for (bytes, name, cat) in cases {
            let got = from_magic(bytes).unwrap_or_else(|| panic!("{name} not recognised"));
            assert_eq!(got.name, *name, "wrong format for {name}");
            assert_eq!(got.category, *cat, "wrong category for {name}");
        }
    }

    #[test]
    fn riff_and_isobmff_containers_are_disambiguated() {
        // All three share the RIFF header and differ only at offset 8.
        assert_eq!(
            from_magic(b"RIFF\x00\x00\x00\x00WAVEfmt ").unwrap().name,
            "wav"
        );
        assert_eq!(
            from_magic(b"RIFF\x00\x00\x00\x00AVI LIST").unwrap().name,
            "avi"
        );
        assert_eq!(
            from_magic(b"RIFF\x00\x00\x00\x00WEBPVP8 ").unwrap().name,
            "webp"
        );
        // ISO-BMFF: brand decides image vs video, both are `ftyp`.
        assert_eq!(
            from_magic(b"\x00\x00\x00\x18ftypavif").unwrap().category,
            Category::Image
        );
        assert_eq!(
            from_magic(b"\x00\x00\x00\x18ftypheic").unwrap().category,
            Category::Image
        );
        assert_eq!(
            from_magic(b"\x00\x00\x00\x18ftypisom").unwrap().category,
            Category::Video
        );
        assert_eq!(
            from_magic(b"\x00\x00\x00\x18ftypM4A ").unwrap().category,
            Category::Audio
        );
    }

    #[test]
    fn tar_is_found_at_its_offset() {
        // ustar lives at 257, not at 0.
        let mut v = pad(b"somefile.txt", 257);
        v.extend_from_slice(b"ustar\x0000");
        v.resize(1024, 0);
        assert_eq!(from_magic(&v).unwrap().name, "tar");
    }

    #[test]
    fn iso9660_is_found_at_its_far_offset() {
        // CD001 sits at 32769, past any reasonable sniff prefix; the detector must
        // handle a buffer that reaches it and not panic on one that does not.
        let mut v = vec![0u8; 32769];
        v.extend_from_slice(b"CD001\x01");
        assert_eq!(from_magic(&v).unwrap().name, "iso9660");
        assert!(
            from_magic(&vec![0u8; 4096]).is_none(),
            "a short buffer must not match"
        );
    }

    #[test]
    fn zip_specialisations_beat_the_generic_container() {
        let mk = |member: &[u8]| {
            let mut v = b"PK\x03\x04\x14\x00\x00\x00\x08\x00".to_vec();
            v.extend_from_slice(member);
            v.resize(512, 0);
            v
        };
        assert_eq!(from_magic(&mk(b"AndroidManifest.xml")).unwrap().name, "apk");
        assert_eq!(from_magic(&mk(b"word/document.xml")).unwrap().name, "docx");
        assert_eq!(from_magic(&mk(b"xl/workbook.xml")).unwrap().name, "xlsx");
        assert_eq!(
            from_magic(&mk(b"ppt/presentation.xml")).unwrap().name,
            "pptx"
        );
        assert_eq!(
            from_magic(&mk(b"META-INF/MANIFEST.MF")).unwrap().name,
            "jar"
        );
        // A plain zip stays a zip.
        assert_eq!(from_magic(&mk(b"readme.txt")).unwrap().name, "zip");
    }

    #[test]
    fn extensions_including_compound_ones_resolve() {
        assert_eq!(
            from_extension("movie.mp4").unwrap().category,
            Category::Video
        );
        assert_eq!(
            from_extension("song.FLAC").unwrap().category,
            Category::Audio,
            "case-insensitive"
        );
        assert_eq!(from_extension("pkg-1.2.tar.gz").unwrap().name, "tar.gz");
        assert_eq!(from_extension("pkg.tar.zst").unwrap().name, "tar.zst");
        assert_eq!(
            from_extension("app.AppImage").unwrap().category,
            Category::Application
        );
        assert_eq!(
            from_extension("disk.qcow2").unwrap().category,
            Category::DiskImage
        );
        // Query strings must not defeat it.
        assert_eq!(
            from_extension("file.zip?token=abc&x=1").unwrap().name,
            "zip"
        );
        assert_eq!(from_extension("no-extension-here"), None);
    }

    #[test]
    fn octet_stream_is_not_a_classification() {
        // The universal "I don't know" must not overwrite better evidence.
        assert!(from_media_type("application/octet-stream").is_none());
        assert!(from_media_type("").is_none());
        assert_eq!(
            from_media_type("video/mp4").unwrap().category,
            Category::Video
        );
        assert_eq!(
            from_media_type("text/html; charset=utf-8")
                .unwrap()
                .category,
            Category::Markup
        );
        assert_eq!(
            from_media_type("audio/ogg").unwrap().category,
            Category::Audio
        );
    }

    #[test]
    fn magic_beats_a_lying_extension() {
        // The single most common real mislabelling: a gzip named .zip.
        let d = detect_format(b"\x1f\x8b\x08\x00", "archive.zip", Some("application/zip"));
        assert_eq!(d.evidence, Evidence::Magic);
        assert_eq!(d.format.unwrap().name, "gzip");
        // Same category, so no conflict is raised.
        assert!(d.conflict.is_none(), "gzip and zip are both archives");
    }

    #[test]
    fn an_html_body_where_an_archive_was_expected_is_flagged() {
        // The captive-portal / error-page signature: status 200, plausible length,
        // and the saved "download" is a login page.
        let d = detect_format(
            b"<!DOCTYPE html><html><head><title>Sign in</title>",
            "ubuntu-24.04.iso",
            Some("text/html"),
        );
        assert_eq!(d.category, Category::Markup);
        assert!(d.conflict.is_some(), "markup vs disk image must conflict");
        assert!(
            d.looks_intercepted(),
            "this is the case a user most needs told about"
        );
        let msg = d.conflict.unwrap();
        assert!(
            msg.contains("html") && msg.contains("iso"),
            "message must name both: {msg}"
        );
    }

    #[test]
    fn html_signatures_allow_preambles_and_case_without_matching_tag_names() {
        for body in [
            b"\xef\xbb\xbf \r\n<HtMl lang='en'>blocked".as_slice(),
            b" \t<!DoCtYpE hTmL><html>blocked",
            b"<!-- gateway -->\n<html>blocked",
            b"<html/>",
            b"<?xml version='1.0'?>\n<html xmlns='http://www.w3.org/1999/xhtml'>",
        ] {
            let detected = detect_format(body, "setup.zip", None);
            assert!(detected.looks_intercepted(), "{body:?}");
            assert_eq!(detected.evidence, Evidence::Magic);
        }
        for body in [
            b"<htmlish>".as_slice(),
            b"<!doctype htmlish>",
            b"<!-- unclosed",
            b"<?xml unclosed",
            b"<html",
        ] {
            assert!(
                !detect_format(body, "setup.zip", None).looks_intercepted(),
                "{body:?}"
            );
        }
        assert_eq!(
            from_magic(b"<?xml version='1.0'?><svg/> ")
                .unwrap()
                .category,
            Category::Data
        );
    }

    #[test]
    fn html_headers_do_not_override_binary_evidence() {
        let probe = detect_format(b"", "setup.zip", Some("text/html"));
        assert_eq!(probe.category, Category::Archive);
        assert_eq!(probe.evidence, Evidence::Extension);
        assert!(!probe.looks_intercepted());
        let download = detect_format(b"PK\x03\x04", "setup.zip", Some("text/html"));
        assert_eq!(download.category, Category::Archive);
        assert!(!download.looks_intercepted());
        let page = detect_format(b"<html>hello", "page.html", Some("text/html"));
        assert!(!page.looks_intercepted());
        assert_eq!(
            from_media_type("Application/XHTML+XML; charset=UTF-8")
                .unwrap()
                .category,
            Category::Markup
        );
    }

    #[test]
    fn a_correct_download_raises_no_conflict() {
        let d = detect_format(b"%PDF-1.7\n%\xc7\xec", "paper.pdf", Some("application/pdf"));
        assert!(d.conflict.is_none());
        assert!(!d.looks_intercepted());
        assert_eq!(d.category, Category::Document);
        assert_eq!(d.category.directory(), "Documents");
    }

    #[test]
    fn falls_back_through_the_evidence_chain() {
        // No payload: extension is next best.
        let d = detect_format(b"", "clip.mkv", None);
        assert_eq!(d.evidence, Evidence::Extension);
        assert_eq!(d.category, Category::Video);
        // No payload and no extension: the header is the last resort.
        let d = detect_format(b"", "stream", Some("audio/mpeg"));
        assert_eq!(d.evidence, Evidence::MediaType);
        assert_eq!(d.category, Category::Audio);
        // Nothing at all.
        let d = detect_format(b"", "stream", None);
        assert_eq!(d.evidence, Evidence::None);
        assert_eq!(d.category, Category::Unknown);
        assert_eq!(d.category.directory(), "Other");
    }

    #[test]
    fn detection_never_panics_on_short_or_empty_input() {
        for n in 0..24usize {
            let buf = vec![0x1fu8; n];
            let _ = from_magic(&buf);
            let _ = detect_format(&buf, "x", Some("application/octet-stream"));
        }
        // And on a buffer that is a strict prefix of a long signature.
        let _ = from_magic(b"SQLite forma");
        let _ = from_magic(b"RIFF");
        let _ = from_magic(b"\x00\x00\x00\x18ftyp");
    }

    #[test]
    fn every_category_has_a_directory_and_a_name() {
        for c in Category::ALL {
            assert!(!c.directory().is_empty(), "{c:?} has no directory");
            assert!(!c.as_str().is_empty(), "{c:?} has no name");
        }
    }

    /// Every format reachable from either table must have a description.
    ///
    /// This is the mechanism that keeps the prose from rotting: adding a signature
    /// without a description fails the build rather than silently shipping a
    /// tooltip that says "Unknown format".
    #[test]
    fn every_format_has_a_description() {
        let mut missing = Vec::new();
        for sig in SIGS {
            if describe(sig.format.name).is_none() {
                missing.push(sig.format.name);
            }
        }
        for (_, f) in ZIP_KINDS {
            if describe(f.name).is_none() {
                missing.push(f.name);
            }
        }
        for (_, f) in BY_EXT {
            if describe(f.name).is_none() {
                missing.push(f.name);
            }
        }
        missing.sort_unstable();
        missing.dedup();
        assert!(
            missing.is_empty(),
            "these formats have no description: {missing:?} — add them to DESCRIPTIONS"
        );
    }

    #[test]
    fn descriptions_are_useful_prose_not_restatements() {
        for (name, label, text) in DESCRIPTIONS {
            assert!(!label.is_empty(), "{name} has an empty label");
            assert!(
                text.len() >= 40,
                "{name}: description is too short to be worth showing: {text:?}"
            );
            assert!(
                text.ends_with('.'),
                "{name}: description should read as a sentence: {text:?}"
            );
            // A description that merely repeats the label teaches nothing.
            assert_ne!(
                text.trim_end_matches('.').to_ascii_lowercase(),
                label.to_ascii_lowercase(),
                "{name}: description just restates the label"
            );
        }
    }

    #[test]
    fn description_table_has_no_duplicate_keys() {
        let mut seen = std::collections::BTreeSet::new();
        for (name, _, _) in DESCRIPTIONS {
            assert!(seen.insert(*name), "duplicate description for {name}");
        }
    }

    #[test]
    fn the_catalogue_covers_every_format_once() {
        let cat = catalogue();
        let mut names: Vec<&str> = cat.iter().map(|(n, ..)| *n).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            before,
            names.len(),
            "the catalogue must not repeat a format"
        );
        // Every entry must be presentable: a GUI shows all of these.
        for (name, _, _, label, text) in &cat {
            assert!(!label.is_empty(), "{name} has no label");
            assert!(!text.is_empty(), "{name} has no description");
        }
        // It must span every category a user can be shown. `Unknown` is the
        // one exception by construction: it is the absence of a
        // classification, so no catalogue entry can carry it.
        for c in Category::ALL
            .into_iter()
            .filter(|c| *c != Category::Unknown)
        {
            assert!(
                cat.iter().any(|(_, cc, ..)| *cc == c),
                "no format in the catalogue for {c:?}"
            );
        }
        assert!(
            cat.len() >= 60,
            "catalogue looks truncated: {} entries",
            cat.len()
        );
    }

    #[test]
    fn known_extensions_are_sorted_unique_and_dotless() {
        let e = known_extensions();
        assert!(e.len() >= 60);
        let mut sorted = e.clone();
        sorted.sort_unstable();
        assert_eq!(
            e, sorted,
            "extensions must come out sorted for a stable UI list"
        );
        for x in &e {
            assert!(!x.starts_with('.'), "{x} should not carry a leading dot");
            assert_eq!(*x, x.to_ascii_lowercase(), "{x} should be lowercase");
        }
    }

    #[test]
    fn hint_reads_as_one_line() {
        let f = from_extension("pkg-1.2.tar.gz").unwrap();
        let h = f.hint();
        assert!(h.starts_with("gzip-compressed tar"), "got {h}");
        assert!(
            h.contains(" — "),
            "label and description must be joined: {h}"
        );
        assert!(!h.contains('\n'), "a hint must fit on one line: {h}");
    }

    #[test]
    fn an_unknown_format_name_degrades_gracefully() {
        // A Format built outside the tables must not panic when described.
        let odd = Format {
            name: "not-a-real-format",
            category: Category::Unknown,
            media_type: "",
            extension: "",
        };
        assert_eq!(odd.label(), "Unknown format");
        assert!(odd.description().contains("No description"));
    }

    #[test]
    fn every_category_has_a_description_that_says_what_to_do() {
        for c in Category::ALL {
            let d = c.description();
            assert!(d.len() >= 40, "{c:?} description too short: {d:?}");
            assert!(d.ends_with('.'), "{c:?} description is not a sentence");
        }
        // The markup case must warn, since that is the one users get wrong.
        assert!(
            Category::Markup.description().contains("login wall")
                || Category::Markup.description().contains("captive portal"),
            "the markup category must explain why an HTML body is suspicious"
        );
    }

    #[test]
    fn the_gzip_description_names_the_single_stream_trap() {
        // The most common real confusion: why a .gz holds only one file.
        let d = describe("gzip").unwrap().1;
        assert!(
            d.contains("SINGLE") || d.contains("single"),
            "gzip's description should explain why .tar.gz exists: {d}"
        );
    }
}
