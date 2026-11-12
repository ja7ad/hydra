//! [`SparseSink`]: positioned writes at exact offsets, no reassembly buffer,
//! resident memory independent of object size.

use crate::stream_digest;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};

/// A file written by positioned writes: no reassembly buffer, no ordering
/// requirement, memory independent of object size.
pub struct SparseSink {
    /// `None` in discard mode: the bytes are measured and dropped, never stored.
    file: Option<std::fs::File>,
    pub written: AtomicU64,
    /// Optional stream observer. Attached when the caller needs the object's
    /// digest but will have no file to hash afterwards (`--no-save`). The sink
    /// is the right home for it: it is the single point every fragment passes
    /// through, so no byte can reach storage without being accounted for.
    digest: Option<std::sync::Mutex<stream_digest::StreamDigest>>,
}

impl SparseSink {
    /// Create or reopen the output as a sparse file of `size` bytes.
    ///
    /// Deliberately does NOT truncate. Two handles are legitimately opened on the same path
    /// in one transfer — the concurrency probe writes real bytes at true offsets, then the
    /// transfer opens the file again — and a truncating reopen silently zeroed everything
    /// the probe had written. Because the scheduler had already marked those ranges held,
    /// they were never refetched: the object arrived with a 1.5 MiB hole of zeros at offset
    /// 0, the right length, and a stable digest. Every length check passed and the file was
    /// unusable, which is the worst failure mode this project keeps finding.
    ///
    /// `set_len` still establishes the full extent (positioned writes need somewhere to
    /// land) and truncates a LONGER pre-existing file, so a stale larger download cannot
    /// leave a tail behind. Callers that want a genuinely fresh file remove it first; that
    /// is what `--force` and the restart path do.
    pub fn create(path: &str, size: u64) -> io::Result<Self> {
        Self::create_observed(path, size, Growth::Measured, |_| true)
    }

    /// [`Self::create`], watching the file being brought to its full length.
    ///
    /// On a filesystem with holes that costs nothing and `on_fill` is never
    /// called. On one without (exFAT, FAT32) every byte has to be written
    /// before a connection may land anywhere in the file, and that is done
    /// here, [`FILL_STEP`] at a time: `on_fill` is told how many bytes of the
    /// file are ready before each step, and returning `false` stops with
    /// [`io::ErrorKind::Interrupted`], leaving a file a later call resumes.
    pub fn create_observed(
        path: &str,
        size: u64,
        growth: Growth,
        mut on_fill: impl FnMut(u64) -> bool,
    ) -> io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            // Explicit, not omitted: this is the load-bearing decision of this function
            // and a future edit must have to think about it.
            .truncate(false)
            .write(true)
            .read(true)
            .open(path)?;
        mark_sparse(&file);
        let have = file.metadata()?.len();
        let free = have >= size
            || size - have <= FILL_STEP
            || (growth == Growth::Measured && grows_for_free(&file, path.as_ref())?);
        if free {
            file.set_len(size)?;
        } else {
            fill(&file, have, size, &mut on_fill)?;
        }
        Ok(SparseSink {
            file: Some(file),
            written: AtomicU64::new(0),
            digest: None,
        })
    }

    /// A sink that accounts for bytes without storing them anywhere.
    ///
    /// This is what `--no-save` needs. The flag used to be implemented as
    /// create-write-digest-delete, which meant the file existed for the whole
    /// transfer: a run interrupted at any point left it behind (measured: 45 MB
    /// surviving a kill), and the flag could not work at all in a directory the
    /// user cannot write to. Nothing about discarding requires a file — the
    /// length, digest, and format classification are all functions of the byte
    /// STREAM, not of a stored copy.
    ///
    /// `write_at` stays a no-op that still counts, so every caller measuring
    /// progress through `written` behaves identically in both modes.
    pub fn discarding() -> Self {
        SparseSink {
            file: None,
            written: AtomicU64::new(0),
            digest: None,
        }
    }

    /// True when this sink stores nothing.
    pub fn is_discarding(&self) -> bool {
        self.file.is_none()
    }

    /// Compute the object's SHA-256 from the stream as it passes through.
    ///
    /// Needed when there will be no file to hash afterwards. Note that ranges
    /// arrive OUT OF ORDER, so this is not a plain rolling hash — see
    /// [`stream_digest`] for how the contiguous prefix is tracked and what
    /// happens when reordering exceeds the buffer budget.
    pub fn with_digest(mut self, cap: usize) -> Self {
        self.digest = Some(std::sync::Mutex::new(stream_digest::StreamDigest::new(cap)));
        self
    }

    /// Stop hashing but keep capturing the head, for a transfer that will open
    /// several spans and so cannot finish the digest (see [`stream_digest`]).
    pub fn abandon_digest(&self) {
        if let Some(Ok(mut g)) = self.digest.as_ref().map(|m| m.lock()) {
            g.abandon();
        }
    }

    /// Take the accumulated digest state, if one was attached.
    ///
    /// Returns `(sha256, head_bytes, unavailable_reason)`. The digest is `None`
    /// when the stream could not be hashed — never a partial or arrival-order
    /// value.
    pub fn take_digest(&self, size: u64) -> Option<(Option<String>, Vec<u8>, Option<String>)> {
        let m = self.digest.as_ref()?;
        let guard = m.lock().ok()?;
        let head = guard.head().to_vec();
        let reason = guard.unavailable_reason(size);
        drop(guard);
        // `finish` consumes, so swap a fresh one in and take ownership.
        let mut g = m.lock().ok()?;
        let taken = std::mem::replace(&mut *g, stream_digest::StreamDigest::new(1));
        Some((taken.finish(size), head, reason))
    }

    /// Write `buf` at absolute offset `off`. Thread-safe without a lock: the
    /// scheduler guarantees ranges are disjoint (the coverage invariant), so no
    /// two connections ever write the same byte.
    pub fn write_at(&self, off: u64, buf: &[u8]) -> io::Result<()> {
        // Observed BEFORE storage so the digest sees every byte in both modes,
        // and sees them even when there is no file at all.
        if let Some(d) = &self.digest {
            if let Ok(mut g) = d.lock() {
                g.write(off, buf);
            }
        }
        match &self.file {
            // Discard mode: count the bytes, store nothing. Deliberately still
            // returns Ok so the transfer, the progress display, and the
            // scheduler's completion accounting are identical in both modes.
            None => {}
            Some(file) => write_all_at(file, buf, off)?,
        }
        // `fetch_add`, not `load` + `store`. Every connection calls `write_at`
        // concurrently, and a read-modify-write split into two separate atomic
        // operations is a lost update: two connections that load the same value
        // both store their own sum, and one write's bytes vanish from the count.
        // The bug is silent and load-shaped — it needs concurrent writes to
        // appear at all, so it undercounts more the faster the transfer goes,
        // which is why a single-connection test could never see it. `written`
        // drives the progress display and the completion check, so an
        // undercount is a transfer that looks unfinished.
        self.written.fetch_add(buf.len() as u64, Ordering::Relaxed);
        Ok(())
    }
}

/// Write all of `buf` at absolute offset `off`, without moving any cursor.
#[inline]
fn write_all_at(file: &std::fs::File, buf: &[u8], off: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.write_all_at(buf, off)
    }
    // Windows: `seek_write` is a positional write (WriteFile with an
    // explicit OVERLAPPED offset) — the offset travels with each call,
    // so concurrent disjoint writes are safe, same as pwrite. A
    // seek-then-write pair is NOT: `seek` moves the ONE cursor all
    // clones of this handle share, so two connections interleave as
    // A-seek, B-seek, A-write and A's block lands at B's offset. That
    // was a real defect: every multi-connection transfer corrupted at
    // block-aligned offsets while single-connection tests stayed green.
    // `seek_write` can write short, so loop like write_all would.
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let (mut buf, mut off) = (buf, off);
        while !buf.is_empty() {
            match file.seek_write(buf, off) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "seek_write returned 0 bytes",
                    ));
                }
                Ok(n) => {
                    buf = &buf[n..];
                    off += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        compile_error!(
            "hydra needs a positional write (pwrite/seek_write): a shared-cursor \
             seek+write fallback silently corrupts multi-connection transfers"
        );
    }
}

/// How much of the file one step of [`fill`] writes. Small enough that a slow
/// USB stick answers a cancel within a second or two.
pub const FILL_STEP: u64 = 4 << 20;

/// How [`SparseSink::create_observed`] learns what growing the file costs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Growth {
    /// Ask the volume: grow the file a step and see whether it took space.
    Measured,
    /// The caller knows the volume has no holes: write every byte.
    Written,
}

/// Whether this filesystem extends a file without storing anything for the
/// new part, measured rather than read off a list of filesystem names: the
/// file is grown by one step and what that did to its allocation is the
/// answer. NTFS with the sparse flag, APFS and ext4 store nothing; exFAT and
/// FAT32 reserve every cluster.
fn grows_for_free(file: &std::fs::File, path: &std::path::Path) -> io::Result<bool> {
    let have = file.metadata()?.len();
    let before = allocated_bytes(path)?;
    file.set_len(have + FILL_STEP)?;
    let grown = allocated_bytes(path)?.saturating_sub(before);
    file.set_len(have)?;
    Ok(grown < FILL_STEP / 2)
}

/// Bring a file on a filesystem without holes from `have` to `size` bytes by
/// writing the zeros itself, in order, a step at a time.
///
/// The filesystem would write them anyway, inside whichever call first lands
/// past them — the `set_len` on FAT32, the first far write on exFAT — and that
/// one call then runs for as long as the whole file takes to write (minutes,
/// for gigabytes on a USB stick), with nothing able to interrupt or report
/// it (#414). Written here they cost the same and can be watched and stopped.
/// The length only ever covers what has been written, so an interrupted fill
/// is resumed from the file's own length.
fn fill(
    file: &std::fs::File,
    have: u64,
    size: u64,
    on_fill: &mut dyn FnMut(u64) -> bool,
) -> io::Result<()> {
    let zeros = vec![0u8; FILL_STEP as usize];
    let mut at = have;
    while at < size {
        if !on_fill(at) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "stopped while allocating disk space",
            ));
        }
        let step = (size - at).min(FILL_STEP) as usize;
        write_all_at(file, &zeros[..step], at)?;
        at += step as u64;
    }
    Ok(())
}

/// Ask NTFS for holes. Without the sparse flag, `set_len` only moves the end of
/// file: the first write far into it makes the kernel zero-fill every byte below,
/// synchronously inside that `WriteFile` — gigabytes of disk writes that block
/// the engine and cannot be cancelled. Best effort: FAT32 and exFAT have no
/// sparse files, and [`fill`] is what they get instead.
#[cfg(windows)]
fn mark_sparse(file: &std::fs::File) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::{Ioctl::FSCTL_SET_SPARSE, IO::DeviceIoControl};
    let mut returned = 0u32;
    // SAFETY: the handle is open for writing and outlives the call; a null input
    // buffer means "set sparse", and FSCTL_SET_SPARSE has no output buffer.
    unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        );
    }
}

#[cfg(not(windows))]
fn mark_sparse(_file: &std::fs::File) {}

/// Bytes the filesystem has actually allocated for `path`, capped at its length.
///
/// The staging file is full-length from its first byte, so its length says nothing
/// about what arrived; for a sparse file the allocation does. On a filesystem without
/// holes this is the full length.
pub fn allocated_bytes(path: &std::path::Path) -> io::Result<u64> {
    let meta = std::fs::metadata(path)?;
    Ok(raw_allocation(path, &meta)?.min(meta.len()))
}

#[cfg(unix)]
fn raw_allocation(_path: &std::path::Path, meta: &std::fs::Metadata) -> io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    Ok(meta.blocks().saturating_mul(512))
}

#[cfg(windows)]
fn raw_allocation(path: &std::path::Path, _meta: &std::fs::Metadata) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{GetLastError, SetLastError, NO_ERROR};
    use windows_sys::Win32::Storage::FileSystem::{GetCompressedFileSizeW, INVALID_FILE_SIZE};
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut high = 0u32;
    // INVALID_FILE_SIZE is also a legitimate low word; only the last error tells the
    // two apart, so clear it first.
    // SAFETY: `wide` is NUL-terminated and outlives the call; `high` is a valid
    // out-pointer; the error calls have no preconditions.
    let (low, err) = unsafe {
        SetLastError(NO_ERROR);
        let low = GetCompressedFileSizeW(wide.as_ptr(), &mut high);
        (low, GetLastError())
    };
    if low == INVALID_FILE_SIZE && err != NO_ERROR {
        return Err(io::Error::from_raw_os_error(err as i32));
    }
    Ok((u64::from(high) << 32) | u64::from(low))
}

#[cfg(not(any(unix, windows)))]
fn raw_allocation(_path: &std::path::Path, meta: &std::fs::Metadata) -> io::Result<u64> {
    Ok(meta.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_abandoned_digest_still_reports_the_head_and_the_reason() {
        let sk = SparseSink::discarding().with_digest(1 << 20);
        sk.abandon_digest();
        sk.write_at(0, b"\x89PNG\r\n\x1a\n").unwrap();
        sk.write_at(4096, &[1; 512]).unwrap();
        let (sha, head, reason) = sk.take_digest(8192).unwrap();
        assert_eq!(sha, None);
        assert!(head.starts_with(b"\x89PNG"));
        assert!(reason.unwrap().contains("out of order"));
    }

    #[test]
    fn abandoning_without_a_digest_is_a_no_op() {
        let sk = SparseSink::discarding();
        sk.abandon_digest();
        sk.write_at(0, &[1; 8]).unwrap();
        assert!(sk.take_digest(8).is_none());
    }

    #[test]
    fn reopening_the_sink_does_not_erase_what_another_handle_wrote() {
        // The bug this guards: the concurrency probe writes real bytes at true offsets, the
        // transfer then opens the same path again, and a truncating reopen zeroed the
        // probe's 1.5 MiB. Those ranges were already marked held, so they were never
        // refetched -- the file had the right length, a stable digest, and a hole of zeros
        // at offset 0. `file` is a shell command away from looking fine.
        let path = std::env::temp_dir().join(format!("hydra_sink_reopen_{}", std::process::id()));
        let p = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&p);

        let first = SparseSink::create(&p, 4096).unwrap();
        first.write_at(0, &[0xAB; 64]).unwrap();
        drop(first);

        let second = SparseSink::create(&p, 4096).unwrap();
        second.write_at(2048, &[0xCD; 64]).unwrap();
        drop(second);

        let got = std::fs::read(&p).unwrap();
        assert_eq!(got.len(), 4096);
        assert_eq!(
            &got[..64],
            &[0xAB; 64],
            "the first handle's bytes must survive the second handle's open"
        );
        assert_eq!(&got[2048..2112], &[0xCD; 64]);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_longer_stale_file_is_cut_back_to_the_new_size() {
        // Not truncating on open must not mean a stale, LONGER download leaves a tail: the
        // extent is set explicitly, which shortens as well as extends.
        let path = std::env::temp_dir().join(format!("hydra_sink_stale_{}", std::process::id()));
        let p = path.to_string_lossy().to_string();
        std::fs::write(&p, vec![0xFF; 8192]).unwrap();
        let s = SparseSink::create(&p, 1024).unwrap();
        drop(s);
        assert_eq!(
            std::fs::metadata(&p).unwrap().len(),
            1024,
            "set_len must cut a longer pre-existing file, or a stale tail survives"
        );
        let _ = std::fs::remove_file(&p);
    }

    trait WriteAtStart {
        fn write_all_at_start(&self, bytes: &[u8]);
    }

    impl WriteAtStart for std::fs::File {
        fn write_all_at_start(&self, bytes: &[u8]) {
            write_all_at(self, bytes, 0).unwrap();
        }
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("hydra_sink_{tag}_{}", std::process::id()))
    }

    #[test]
    fn a_far_write_allocates_only_the_bytes_written() {
        // Issue #414: on NTFS without the sparse flag, this one 64 KiB write near the
        // end made the kernel write 256 MiB of zeros below it before returning.
        let path = scratch("far_write");
        let _ = std::fs::remove_file(&path);
        const SIZE: u64 = 256 << 20;
        let sink = SparseSink::create(&path.to_string_lossy(), SIZE).unwrap();
        sink.write_at(SIZE - (64 << 10), &[0x5A; 64 << 10]).unwrap();
        drop(sink);

        let allocated = allocated_bytes(&path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), SIZE);
        assert!(
            allocated < 16 << 20,
            "a staging file must stay sparse below its first write, got {allocated} bytes allocated"
        );
        let _ = std::fs::remove_file(&path);
    }

    fn open_scratch(tag: &str) -> (std::path::PathBuf, std::fs::File) {
        let path = scratch(tag);
        let _ = std::fs::remove_file(&path);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        (path, file)
    }

    #[test]
    fn a_filesystem_with_holes_is_never_filled() {
        let path = scratch("holes");
        let _ = std::fs::remove_file(&path);
        let mut steps = 0;
        let sink = SparseSink::create_observed(
            &path.to_string_lossy(),
            64 << 20,
            Growth::Measured,
            |_| {
                steps += 1;
                true
            },
        );
        drop(sink.unwrap());
        // The scratch directory may itself be on a volume without holes.
        if allocated_bytes(&path).unwrap() < 32 << 20 {
            assert_eq!(steps, 0);
        }
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 64 << 20);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn measuring_how_a_file_grows_leaves_it_as_it_was() {
        let (path, file) = open_scratch("probe");
        file.write_all_at_start(b"kept");
        let free = grows_for_free(&file, &path).unwrap();
        assert_eq!(file.metadata().unwrap().len(), 4);
        assert_eq!(std::fs::read(&path).unwrap(), b"kept");
        // Whatever the answer here, it is the one the allocation gives.
        file.set_len(4 + FILL_STEP).unwrap();
        assert_eq!(free, allocated_bytes(&path).unwrap() < FILL_STEP / 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_fill_reports_each_step_and_ends_at_exactly_the_size() {
        let (path, file) = open_scratch("fill");
        const SIZE: u64 = 2 * FILL_STEP + 123;
        let mut seen = Vec::new();
        fill(&file, 0, SIZE, &mut |at| {
            seen.push(at);
            true
        })
        .unwrap();
        assert_eq!(seen, [0, FILL_STEP, 2 * FILL_STEP]);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len() as u64, SIZE);
        assert!(bytes.iter().all(|b| *b == 0));
        let _ = std::fs::remove_file(&path);
    }

    /// #414: Pause and Cancel did nothing while the file was being written
    /// out, and the writing started over on the next attempt.
    #[test]
    fn a_stopped_fill_keeps_what_it_wrote_and_is_resumed_from_there() {
        let (path, file) = open_scratch("stopped");
        const SIZE: u64 = 3 * FILL_STEP;
        let stopped = fill(&file, 0, SIZE, &mut |at| at < 2 * FILL_STEP).unwrap_err();
        assert_eq!(stopped.kind(), io::ErrorKind::Interrupted);
        let have = file.metadata().unwrap().len();
        assert_eq!(
            have,
            2 * FILL_STEP,
            "the length covers only what was written"
        );

        let mut resumed = Vec::new();
        fill(&file, have, SIZE, &mut |at| {
            resumed.push(at);
            true
        })
        .unwrap();
        assert_eq!(resumed, [2 * FILL_STEP], "nothing is written twice");
        assert_eq!(file.metadata().unwrap().len(), SIZE);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_fill_never_touches_bytes_already_in_the_file() {
        let (path, file) = open_scratch("kept");
        file.write_all_at_start(&[0xAB; 4096]);
        fill(&file, 4096, 4096 + FILL_STEP, &mut |_| true).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes[..4096].iter().all(|b| *b == 0xAB));
        assert!(bytes[4096..].iter().all(|b| *b == 0));
        let _ = std::fs::remove_file(&path);
    }

    /// What a volume without holes is made to do, on whatever volume the
    /// tests run on. Name a real one to measure the far write there:
    /// `HYDRA_NO_HOLES_DIR=/Volumes/EXFAT cargo test -p hya-net no_holes`.
    #[test]
    fn on_a_volume_with_no_holes_the_file_is_filled_in_steps_and_then_writes_anywhere() {
        let real = std::env::var_os("HYDRA_NO_HOLES_DIR");
        let growth = if real.is_some() {
            Growth::Measured
        } else {
            Growth::Written
        };
        let dir = real.map_or_else(std::env::temp_dir, std::path::PathBuf::from);
        let path = dir.join(format!("hydra_no_holes_{}.part", std::process::id()));
        let _ = std::fs::remove_file(&path);
        const SIZE: u64 = 64 << 20;
        let name = path.to_string_lossy().into_owned();

        let stopped = SparseSink::create_observed(&name, SIZE, growth, |at| at < SIZE / 2);
        assert_eq!(stopped.err().unwrap().kind(), io::ErrorKind::Interrupted);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), SIZE / 2);

        let mut steps = Vec::new();
        let sink = SparseSink::create_observed(&name, SIZE, growth, |at| {
            steps.push(at);
            true
        })
        .unwrap();
        assert_eq!(steps.first(), Some(&(SIZE / 2)), "resumed, not restarted");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), SIZE);

        let far = std::time::Instant::now();
        sink.write_at(SIZE - (64 << 10), &[0x5A; 64 << 10]).unwrap();
        assert!(
            far.elapsed() < std::time::Duration::from_secs(5),
            "a far write still filled the file below it: {:?}",
            far.elapsed()
        );
        drop(sink);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_fully_written_file_reports_its_whole_length_allocated() {
        let path = scratch("full");
        std::fs::write(&path, vec![0x11; 3 * 4096 + 5]).unwrap();
        assert_eq!(allocated_bytes(&path).unwrap(), 3 * 4096 + 5);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn allocation_of_a_missing_file_is_an_error() {
        assert!(allocated_bytes(&scratch("missing")).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn the_staging_file_carries_the_sparse_attribute() {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_SPARSE_FILE;
        let path = scratch("attr");
        let _ = std::fs::remove_file(&path);
        drop(SparseSink::create(&path.to_string_lossy(), 1 << 20).unwrap());
        let attrs = std::fs::metadata(&path).unwrap().file_attributes();
        assert_ne!(attrs & FILE_ATTRIBUTE_SPARSE_FILE, 0);
        let _ = std::fs::remove_file(&path);
    }
}
