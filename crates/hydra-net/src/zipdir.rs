// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A ZIP archive's table of contents, read from its tail.
//!
//! ZIP keeps its index — the *central directory* — at the END of the file,
//! followed by a small "end of central directory" record that says where the
//! directory starts and how long it is. That layout is what makes a remote
//! archive listable without downloading it: one ranged GET for the last few
//! tens of kilobytes finds the directory, and for most archives already
//! contains it. A download manager can therefore show what is inside a
//! multi-gigabyte archive before the user commits to it.
//!
//! The parsing is pure: [`locate`] reads the end record out of the tail and
//! says where the directory is; [`entries`] turns the directory bytes into a
//! list. Two calls rather than one because the directory may not fit in the
//! tail fetched speculatively. [`fetch_listing`] is the one caller of both
//! that talks to a server — the tail, then the directory if it was not in
//! the tail — so the GUI's Preview button and the CLI's `--preview` list an
//! archive the same way and differ only in how they draw the result.
//!
//! Handles the parts of the format a listing needs and no more: ZIP64
//! (archives past 4 GiB or 65 535 entries), archive comments, data prepended
//! before the archive (self-extractors), UTF-8 names by flag or by the
//! Info-ZIP Unicode Path field, and CP437 for names that are neither. It does
//! not decompress anything.

use crate::{Connector, Target};
use std::fmt;
use std::io;

/// How much of the file's tail is worth fetching before looking.
///
/// The end record is 22 bytes plus an archive comment of at most 65 535,
/// and ZIP64 adds a 20-byte locator and a 56-byte record ahead of it. This
/// covers every legal placement, so a tail this long always contains the end
/// record — and, for any archive with fewer than a few hundred entries, the
/// whole directory as well.
pub const TAIL_LEN: u64 = 66 * 1024;

/// Sanity ceiling for a directory this module will parse: ~46 bytes plus a
/// name per entry, so 64 MiB is on the order of a million entries.
pub const MAX_DIRECTORY_LEN: u64 = 64 * 1024 * 1024;

const EOCD_SIG: u32 = 0x0605_4b50;
const EOCD64_LOCATOR_SIG: u32 = 0x0706_4b50;
const EOCD64_SIG: u32 = 0x0606_4b50;
const CENTRAL_HEADER_SIG: u32 = 0x0201_4b50;

const EOCD_LEN: usize = 22;
const EOCD64_LOCATOR_LEN: usize = 20;
const EOCD64_MIN_LEN: usize = 56;

/// Why the bytes could not be read as an archive index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// No end-of-central-directory record in the tail: whatever this file is,
    /// it is not a ZIP archive (or the tail given was shorter than [`TAIL_LEN`]
    /// and the archive carries an unusually long comment).
    NotZip,
    /// The end record was found but describes a directory that cannot exist
    /// in a file of this size.
    Corrupt(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotZip => f.write_str("not a ZIP archive"),
            Error::Corrupt(why) => write!(f, "corrupt ZIP archive: {why}"),
        }
    }
}

impl std::error::Error for Error {}

/// Where the central directory sits inside the archive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Directory {
    /// Byte offset of the first central header, from the START of the file.
    pub offset: u64,
    /// Length of the directory in bytes.
    pub len: u64,
    /// Entry count the end record states. Advisory: [`entries`] reads to the
    /// end of the directory rather than trusting this.
    pub count: u64,
}

/// A local date and time as ZIP stores it: two-second resolution, no time
/// zone (the archiver's wall clock), years 1980 through 2107.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DosTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl DosTime {
    fn from_dos(time: u16, date: u16) -> Option<Self> {
        if date == 0 {
            return None;
        }
        let day = (date & 0x1f) as u8;
        let month = ((date >> 5) & 0x0f) as u8;
        let year = 1980 + (date >> 9);
        let second = ((time & 0x1f) * 2) as u8;
        let minute = ((time >> 5) & 0x3f) as u8;
        let hour = (time >> 11) as u8;
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
            return None;
        }
        Some(Self {
            year,
            month,
            day,
            hour,
            minute,
            second: second.min(59),
        })
    }
}

/// One file (or directory) in the archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Path inside the archive, `/`-separated; a directory ends in `/`.
    pub name: String,
    /// Uncompressed size.
    pub size: u64,
    /// Compressed size as stored.
    pub packed: u64,
    /// Last-modified stamp, when the archiver recorded one.
    pub modified: Option<DosTime>,
    /// The entry's data is encrypted (its name is still readable: only a
    /// self-extractor-style wrapper would hide the directory itself).
    pub encrypted: bool,
    /// Offset of the entry's local header from the start of the file. Kept
    /// so a caller could fetch one entry's bytes on their own later.
    pub local_header: u64,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.name.ends_with('/')
    }
}

/// Does this file NAME say ZIP? Extensions the format hides behind, in the
/// sense that the archive inside is a plain ZIP: Java, Android and Firefox
/// packages, EPUB books, comic-book archives.
pub fn is_zip_name(name: &str) -> bool {
    let stem = name.split(['?', '#']).next().unwrap_or(name);
    let ext = stem.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "zip" | "jar" | "apk" | "xpi" | "epub" | "cbz" | "war" | "ipa"
    )
}

/// Find the central directory from the archive's tail.
///
/// `tail` is the last `tail.len()` bytes of a file `total` bytes long; the
/// caller normally fetched [`TAIL_LEN`] of them (or the whole file when it is
/// shorter). Returns where the directory is, in absolute file offsets, so
/// the caller can slice it out of `tail` when it is already there or fetch
/// exactly that span when it is not.
pub fn locate(tail: &[u8], total: u64) -> Result<Directory, Error> {
    if tail.len() as u64 > total {
        return Err(Error::Corrupt("tail longer than the file"));
    }
    let tail_start = total - tail.len() as u64;
    let eocd = find_eocd(tail).ok_or(Error::NotZip)?;
    let rec = &tail[eocd..eocd + EOCD_LEN];
    let mut count = u16_at(rec, 10) as u64;
    let mut len = u32_at(rec, 12) as u64;
    let mut offset = u32_at(rec, 16) as u64;
    // The directory ends where the end record begins — unless a ZIP64 record
    // sits between them, in which case it ends where THAT begins.
    let mut dir_end = tail_start + eocd as u64;

    // ZIP64: a locator immediately precedes the end record and points at the
    // 64-bit record. Some archivers write one whenever they feel like it, so
    // its presence rather than a 0xFFFF sentinel is what decides.
    if eocd >= EOCD64_LOCATOR_LEN {
        let loc = &tail[eocd - EOCD64_LOCATOR_LEN..eocd];
        if u32_at(loc, 0) == EOCD64_LOCATOR_SIG {
            let rec64_abs = u64_at(loc, 8);
            let locator_abs = tail_start + (eocd - EOCD64_LOCATOR_LEN) as u64;
            // The 64-bit record must lie inside the tail we hold, before the
            // locator. A record far from its locator would mean prepended
            // data of a kind this module does not correct for — treat the
            // 32-bit values as authoritative in that case rather than fail.
            if rec64_abs >= tail_start && rec64_abs + (EOCD64_MIN_LEN as u64) <= locator_abs {
                let p = (rec64_abs - tail_start) as usize;
                let r64 = &tail[p..p + EOCD64_MIN_LEN];
                if u32_at(r64, 0) == EOCD64_SIG {
                    count = u64_at(r64, 32);
                    len = u64_at(r64, 40);
                    offset = u64_at(r64, 48);
                    dir_end = rec64_abs;
                }
            }
        }
    }

    if len > dir_end {
        return Err(Error::Corrupt("central directory longer than the file"));
    }
    // Data prepended before the archive (a self-extractor stub, or a file
    // concatenated in front) shifts every stored offset by its length. The
    // directory's END is known exactly, so its start is END - LEN whatever
    // the record claims; the claim is only checked for plausibility.
    let actual = dir_end - len;
    if offset > actual {
        return Err(Error::Corrupt("central directory offset past its end"));
    }
    Ok(Directory {
        offset: actual,
        len,
        count,
    })
}

/// Parse the central directory into its entries.
///
/// `dir` is exactly the bytes [`locate`] described. Reads to the end of the
/// bytes, stopping early at the digital-signature record some archivers
/// append, and tolerates a truncated final header rather than failing the
/// whole listing on it.
pub fn entries(dir: &[u8]) -> Result<Vec<Entry>, Error> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p + 46 <= dir.len() {
        let h = &dir[p..p + 46];
        if u32_at(h, 0) != CENTRAL_HEADER_SIG {
            if out.is_empty() {
                return Err(Error::Corrupt(
                    "central directory does not start with a header",
                ));
            }
            break;
        }
        let flags = u16_at(h, 8);
        let time = u16_at(h, 12);
        let date = u16_at(h, 14);
        let mut packed = u32_at(h, 20) as u64;
        let mut size = u32_at(h, 24) as u64;
        let name_len = u16_at(h, 28) as usize;
        let extra_len = u16_at(h, 30) as usize;
        let comment_len = u16_at(h, 32) as usize;
        let mut local_header = u32_at(h, 42) as u64;

        let name_at = p + 46;
        let extra_at = name_at + name_len;
        let next = extra_at + extra_len + comment_len;
        if next > dir.len() {
            break;
        }
        let raw_name = &dir[name_at..extra_at];
        let extra = &dir[extra_at..extra_at + extra_len];

        let mut unicode_name = None;
        for (id, data) in extra_fields(extra) {
            match id {
                // ZIP64 extended information: only the fields whose 32-bit
                // counterparts are saturated are present, in a fixed order.
                0x0001 => {
                    let mut q = 0usize;
                    let mut take = |want: bool| -> Option<u64> {
                        if !want {
                            return None;
                        }
                        let v = (q + 8 <= data.len()).then(|| u64_at(data, q));
                        q += 8;
                        v
                    };
                    if let Some(v) = take(size == u32::MAX as u64) {
                        size = v;
                    }
                    if let Some(v) = take(packed == u32::MAX as u64) {
                        packed = v;
                    }
                    if let Some(v) = take(local_header == u32::MAX as u64) {
                        local_header = v;
                    }
                }
                // Info-ZIP Unicode Path: version byte, CRC-32 of the header
                // name, then the UTF-8 name. Written by WinRAR and Info-ZIP
                // for names the local code page cannot hold.
                0x7075 if data.len() > 5 && data[0] == 1 => {
                    if let Ok(s) = std::str::from_utf8(&data[5..]) {
                        unicode_name = Some(s.to_string());
                    }
                }
                _ => {}
            }
        }

        let name = match unicode_name {
            Some(n) => n,
            None => decode_name(raw_name, flags & 0x0800 != 0),
        };
        out.push(Entry {
            name,
            size,
            packed,
            modified: DosTime::from_dos(time, date),
            encrypted: flags & 0x0001 != 0,
            local_header,
        });
        p = next;
    }
    Ok(out)
}

/// Why a remote listing could not be produced.
#[derive(Debug)]
pub enum PeekError {
    /// A request failed. A redirect surfaces here too, as
    /// [`crate::Redirect`] inside the error, for the caller that owns the URL
    /// to follow — this function has a target, not an address.
    Net(io::Error),
    /// The server answered a `Range` request with the whole object: the
    /// archive cannot be listed without downloading it.
    NoRanges,
    /// The bytes are not a ZIP index, or not a sane one.
    Zip(Error),
    /// The directory exceeds [`MAX_DIRECTORY_LEN`].
    IndexTooLarge,
}

impl fmt::Display for PeekError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PeekError::Net(e) => write!(f, "{e}"),
            PeekError::NoRanges => f.write_str(
                "the server does not support partial downloads, so the archive cannot be \
                 listed without downloading it",
            ),
            PeekError::Zip(e) => write!(f, "{e}"),
            PeekError::IndexTooLarge => f.write_str("the archive's index is too large to list"),
        }
    }
}

impl std::error::Error for PeekError {}

/// List a remote archive of `total` bytes at `t` without downloading it.
///
/// One ranged GET for the tail, and a second for the directory only when
/// the tail did not already hold it — an archive with thousands of entries.
/// An object no longer than [`TAIL_LEN`] is fetched whole, unranged: a
/// server that ignores `Range` cannot then hand back the wrong bytes, and a
/// tiny archive costs one request either way.
///
/// `total` must be the object's true size; it is the only way to know
/// where the tail starts. Callers have it from the probe that found the
/// object — or, in the GUI, from the transfer already running behind the
/// dialog, which is what lets the listing skip the probe altogether.
pub async fn fetch_listing<C: Connector>(
    c: &C,
    t: &Target,
    total: u64,
) -> Result<Vec<Entry>, PeekError> {
    let tail = if total <= TAIL_LEN {
        crate::http::fetch_small(c, t, total as usize).await
    } else {
        crate::http::fetch_small_range(c, t, total - TAIL_LEN, total - 1, TAIL_LEN as usize).await
    }
    .map_err(net)?;
    let dir = locate(&tail, total).map_err(PeekError::Zip)?;
    let tail_start = total - tail.len() as u64;
    let bytes = if dir.offset >= tail_start {
        let lo = (dir.offset - tail_start) as usize;
        let hi = (lo as u64 + dir.len).min(tail.len() as u64) as usize;
        tail[lo..hi].to_vec()
    } else {
        if dir.len > MAX_DIRECTORY_LEN {
            return Err(PeekError::IndexTooLarge);
        }
        crate::http::fetch_small_range(c, t, dir.offset, dir.offset + dir.len - 1, dir.len as usize)
            .await
            .map_err(net)?
    };
    entries(&bytes).map_err(PeekError::Zip)
}

/// A refused `Range` is its own outcome; every other transport failure is
/// passed through with the redirect, if any, still inside it.
fn net(e: io::Error) -> PeekError {
    if e.to_string().contains("ignored the Range") {
        PeekError::NoRanges
    } else {
        PeekError::Net(e)
    }
}

/// Position of the end-of-central-directory record inside `tail`.
///
/// Scanned backwards, and a candidate is accepted only if its comment-length
/// field puts the end of the record at the end of the file — the signature
/// bytes can legitimately occur inside a comment or inside compressed data.
fn find_eocd(tail: &[u8]) -> Option<usize> {
    if tail.len() < EOCD_LEN {
        return None;
    }
    let mut p = tail.len() - EOCD_LEN;
    loop {
        if u32_at(tail, p) == EOCD_SIG {
            let comment_len = u16_at(tail, p + 20) as usize;
            if p + EOCD_LEN + comment_len == tail.len() {
                return Some(p);
            }
        }
        if p == 0 {
            return None;
        }
        p -= 1;
    }
}

fn extra_fields(mut extra: &[u8]) -> impl Iterator<Item = (u16, &[u8])> {
    std::iter::from_fn(move || {
        if extra.len() < 4 {
            return None;
        }
        let id = u16_at(extra, 0);
        let len = u16_at(extra, 2) as usize;
        if 4 + len > extra.len() {
            return None;
        }
        let data = &extra[4..4 + len];
        extra = &extra[4 + len..];
        Some((id, data))
    })
}

/// A name is UTF-8 when the archive says so — and, in practice, whenever the
/// bytes happen to be valid UTF-8, because most modern archivers write UTF-8
/// without setting the flag. Anything else is CP437, the format's default.
fn decode_name(raw: &[u8], utf8_flag: bool) -> String {
    if utf8_flag {
        return String::from_utf8_lossy(raw).into_owned();
    }
    match std::str::from_utf8(raw) {
        Ok(s) => s.to_string(),
        Err(_) => raw
            .iter()
            .map(|&b| {
                if b < 0x80 {
                    b as char
                } else {
                    CP437_HIGH[(b - 0x80) as usize]
                }
            })
            .collect(),
    }
}

/// Code page 437, bytes 0x80..=0xFF.
const CP437_HIGH: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', 'É', 'æ', 'Æ',
    'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ', 'á', 'í', 'ó', 'ú', 'ñ', 'Ñ',
    'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»', '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕',
    '╣', '║', '╗', '╝', '╜', '╛', '┐', '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦',
    '╠', '═', '╬', '╧', '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐',
    '▀', 'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩', '≡', '±',
    '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', '\u{a0}',
];

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn u64_at(b: &[u8], i: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[i..i + 8]);
    u64::from_le_bytes(a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    /// A real archive, written by the `zip` crate so the fixture is what
    /// archivers actually produce rather than what this parser expects.
    fn archive(comment: Option<&str>, zip64: bool) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        if let Some(c) = comment {
            w.set_comment(c).unwrap();
        }
        let stamp = zip::DateTime::from_date_and_time(2026, 9, 4, 13, 27, 30).unwrap();
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .last_modified_time(stamp)
            .large_file(zip64);
        w.add_directory("hydra-0.4.1/", opts).unwrap();
        w.start_file("hydra-0.4.1/README.md", opts).unwrap();
        w.write_all(&vec![b'a'; 5000]).unwrap();
        w.start_file("hydra-0.4.1/کلم بروکلی.txt", opts).unwrap();
        w.write_all(b"salad").unwrap();
        w.start_file(
            "hydra-0.4.1/bin/hydra",
            opts.compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        w.write_all(&[7u8; 300]).unwrap();
        w.finish().unwrap().into_inner()
    }

    /// The listing as a caller would produce it: fetch a tail, locate, slice
    /// or fetch the directory, parse.
    fn list(file: &[u8]) -> Result<Vec<Entry>, Error> {
        let total = file.len() as u64;
        let tail_len = total.min(TAIL_LEN) as usize;
        let tail = &file[file.len() - tail_len..];
        let dir = locate(tail, total)?;
        let bytes = &file[dir.offset as usize..(dir.offset + dir.len) as usize];
        entries(bytes)
    }

    #[test]
    fn lists_names_sizes_and_stamps() {
        let file = archive(None, false);
        let got = list(&file).unwrap();
        let names: Vec<&str> = got.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "hydra-0.4.1/",
                "hydra-0.4.1/README.md",
                "hydra-0.4.1/کلم بروکلی.txt",
                "hydra-0.4.1/bin/hydra",
            ]
        );
        assert!(got[0].is_dir());
        assert_eq!(got[1].size, 5000);
        assert!(got[1].packed < 100, "5000 'a's deflate to a few bytes");
        assert_eq!(got[3].size, 300);
        assert_eq!(got[3].packed, 300, "stored entry");
        assert_eq!(
            got[1].modified,
            Some(DosTime {
                year: 2026,
                month: 9,
                day: 4,
                hour: 13,
                minute: 27,
                second: 30,
            })
        );
        assert!(got.iter().all(|e| !e.encrypted));
    }

    #[test]
    fn locate_reports_the_directory_span() {
        let file = archive(None, false);
        let dir = locate(&file, file.len() as u64).unwrap();
        assert_eq!(dir.count, 4);
        assert_eq!(u32_at(&file, dir.offset as usize), CENTRAL_HEADER_SIG);
        // The directory runs right up to the end record.
        assert_eq!(u32_at(&file, (dir.offset + dir.len) as usize), EOCD_SIG);
    }

    #[test]
    fn a_tail_shorter_than_the_directory_still_locates_it() {
        let file = archive(None, false);
        let total = file.len() as u64;
        // Just the end record: locate must work, and point outside the tail.
        let tail = &file[file.len() - EOCD_LEN..];
        let dir = locate(tail, total).unwrap();
        assert!(dir.offset < total - EOCD_LEN as u64);
        let bytes = &file[dir.offset as usize..(dir.offset + dir.len) as usize];
        assert_eq!(entries(bytes).unwrap().len(), 4);
    }

    #[test]
    fn archive_comment_does_not_hide_the_end_record() {
        // A comment that itself contains the end-record signature.
        let comment = format!("PK\x05\x06 in a comment {}", "x".repeat(300));
        let file = archive(Some(&comment), false);
        assert_eq!(list(&file).unwrap().len(), 4);
    }

    #[test]
    fn zip64_records_are_read() {
        let file = archive(None, true);
        let dir = locate(&file, file.len() as u64).unwrap();
        assert_eq!(dir.count, 4);
        let got = list(&file).unwrap();
        assert_eq!(got[1].size, 5000);
        assert_eq!(got[3].size, 300);
        assert_eq!(got[2].name, "hydra-0.4.1/کلم بروکلی.txt");
    }

    #[test]
    fn prepended_data_is_corrected_for() {
        // A self-extractor: a stub in front of the archive shifts every
        // stored offset, but the directory still ends at the end record.
        let mut file = b"MZ this is a 1234-byte stub ".repeat(44);
        let plain = archive(None, false);
        file.extend_from_slice(&plain);
        let got = list(&file).unwrap();
        assert_eq!(got.len(), 4);
        assert_eq!(got[1].name, "hydra-0.4.1/README.md");
    }

    #[test]
    fn not_a_zip_is_reported_as_such() {
        let junk = vec![0x41u8; 4000];
        assert_eq!(locate(&junk, 4000).unwrap_err(), Error::NotZip);
        // A gzip tail, say.
        let tiny = [0x1f, 0x8b, 0x08, 0, 0, 0, 0, 0, 0, 3];
        assert_eq!(locate(&tiny, 10).unwrap_err(), Error::NotZip);
    }

    #[test]
    fn a_lying_end_record_is_corrupt_not_a_panic() {
        let mut file = archive(None, false);
        let n = file.len();
        // Directory length larger than the whole file.
        file[n - 10..n - 6].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(locate(&file, n as u64), Err(Error::Corrupt(_))));
    }

    #[test]
    fn cp437_names_decode_and_utf8_is_recognised_without_the_flag() {
        assert_eq!(decode_name(b"caf\x82.txt", false), "café.txt");
        assert_eq!(decode_name("naïve.txt".as_bytes(), false), "naïve.txt");
        assert_eq!(decode_name("naïve.txt".as_bytes(), true), "naïve.txt");
    }

    #[test]
    fn zip_names() {
        assert!(is_zip_name("Mass.Downloader.zip"));
        assert!(is_zip_name("app.APK"));
        assert!(is_zip_name("https://x/y/book.epub?dl=1"));
        assert!(!is_zip_name("hydra.tar.gz"));
        assert!(!is_zip_name("video.mp4"));
        assert!(!is_zip_name("noext"));
    }

    /// An origin that answers a ranged GET with exactly the span asked for,
    /// counting the body bytes it sends: the number the feature is about.
    fn serve(object: Vec<u8>) -> (u16, std::sync::Arc<std::sync::atomic::AtomicU64>) {
        use std::io::{BufRead, BufReader};
        use std::sync::atomic::{AtomicU64, Ordering};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().unwrap().port();
        let sent = std::sync::Arc::new(AtomicU64::new(0));
        let counter = sent.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut sock) = conn else { continue };
                let Ok(peek) = sock.try_clone() else { continue };
                let mut r = BufReader::new(peek);
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let mut range = None;
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                        break;
                    }
                    if let Some(v) = h.strip_prefix("Range: bytes=") {
                        let (lo, hi) = v.trim().split_once('-').unwrap();
                        range = Some((lo.parse::<usize>().unwrap(), hi.parse::<usize>().unwrap()));
                    }
                }
                let total = object.len();
                let (head, body): (String, &[u8]) = match range {
                    Some((lo, hi)) if !line.starts_with("GET /deaf") => {
                        let hi = hi.min(total - 1);
                        (
                            format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {lo}-{hi}/{total}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", hi - lo + 1),
                            &object[lo..=hi],
                        )
                    }
                    _ => (
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n"),
                        &object[..],
                    ),
                };
                let _ = sock.write_all(head.as_bytes());
                let _ = sock.write_all(body);
                counter.fetch_add(body.len() as u64, Ordering::SeqCst);
                let _ = sock.flush();
            }
        });
        (port, sent)
    }

    /// Data-heavy like real archives, with enough long-named entries that
    /// the directory does NOT fit in the tail: the listing has to come back
    /// for it.
    fn big_archive(entries: usize) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for i in 0..entries {
            w.start_file(format!("dir/{}/{i:05}.bin", "x".repeat(180)), opts)
                .unwrap();
            w.write_all(&[i as u8; 4096]).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[tokio::test]
    async fn lists_a_remote_archive_without_downloading_it() {
        let file = big_archive(600);
        let total = file.len() as u64;
        assert!(total > 3 * TAIL_LEN, "fixture must dwarf the tail");
        let (port, sent) = serve(file);
        let t = Target::direct("127.0.0.1", port, "/big.zip");
        let got = fetch_listing(&crate::TcpConnector, &t, total)
            .await
            .expect("listing");
        assert_eq!(got.len(), 600);
        assert!(got[599].name.ends_with("00599.bin"));
        assert_eq!(got[0].size, 4096);
        let moved = sent.load(std::sync::atomic::Ordering::SeqCst);
        assert!(moved < total / 2, "moved {moved} of {total} bytes");
        assert!(moved > TAIL_LEN, "the directory was fetched separately");
    }

    #[tokio::test]
    async fn a_small_archive_is_fetched_whole_and_a_deaf_server_is_named() {
        let file = archive(None, false);
        let total = file.len() as u64;
        let (port, _) = serve(file.clone());
        let t = Target::direct("127.0.0.1", port, "/small.zip");
        let got = fetch_listing(&crate::TcpConnector, &t, total)
            .await
            .expect("listing");
        assert_eq!(got.len(), 4);

        // Big enough to need a range, from a path that ignores Range.
        let big = big_archive(50);
        let total = big.len() as u64;
        let (port, _) = serve(big);
        let t = Target::direct("127.0.0.1", port, "/deaf.zip");
        assert!(matches!(
            fetch_listing(&crate::TcpConnector, &t, total).await,
            Err(PeekError::NoRanges)
        ));

        let (port, _) = serve(vec![b'x'; 200_000]);
        let t = Target::direct("127.0.0.1", port, "/not.zip");
        assert!(matches!(
            fetch_listing(&crate::TcpConnector, &t, 200_000).await,
            Err(PeekError::Zip(Error::NotZip))
        ));
    }

    #[test]
    fn dos_time_rejects_nonsense() {
        assert_eq!(DosTime::from_dos(0, 0), None);
        // Month 13.
        assert_eq!(DosTime::from_dos(0, 13 << 5 | 1), None);
        let t =
            DosTime::from_dos(13 << 11 | 27 << 5 | 15, (2026 - 1980) << 9 | 9 << 5 | 4).unwrap();
        assert_eq!(
            (t.year, t.month, t.day, t.hour, t.minute, t.second),
            (2026, 9, 4, 13, 27, 30)
        );
    }
}
