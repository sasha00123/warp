//! Connection-independent terminal journal with bounded hot storage and optional compressed history.
//!
//! A tmux pipe owns the recorder process. The SSH daemon only reads snapshots.
//! A cursor is a byte offset within one workspace incarnation, never a tmux ID.
//! Published bytes are immutable; a reader never observes an uncommitted tail.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use flate2::{Compression, read::GzDecoder, write::GzEncoder};

pub const RECORDER_ARGUMENT: &str = "--persistent-workspace-recorder";
pub const SEGMENT_BYTES: u64 = 1024 * 1024;
pub const RETAINED_SEGMENTS: u64 = 64;
pub const MAX_PAGE_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RetentionPolicy {
    #[default]
    UntilWorkspaceDeleted,
    Rolling,
}

#[derive(Clone, Copy, Debug)]
struct Manifest {
    segment_bytes: u64,
    retained_segments: u64,
    first: u64,
    end: u64,
    closed: bool,
    archive: bool,
    archived_bytes: u64,
}

/// `closed` means clean EOF, not that the shell is idle or has exited.
/// A missing recorder must be detected separately by the transport owner.
#[derive(Debug, PartialEq, Eq)]
pub struct OutputPage {
    pub start_cursor: u64,
    pub next_cursor: u64,
    pub earliest_cursor: u64,
    pub high_watermark: u64,
    pub history_gap: bool,
    pub closed: bool,
    pub bytes: Vec<u8>,
}

pub struct Recorder {
    directory: PathBuf,
    manifest: Manifest,
    segment: Option<File>,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn private_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
}

fn segment_path(directory: &Path, index: u64) -> PathBuf {
    directory.join(format!("{index:016x}.output"))
}

fn archive_path(directory: &Path, index: u64) -> PathBuf {
    directory.join(format!("{index:016x}.output.gz"))
}

fn archive_segment(directory: &Path, index: u64, segment_bytes: u64) -> io::Result<u64> {
    let source = File::open(segment_path(directory, index))?;
    if source.metadata()?.len() != segment_bytes {
        return Err(invalid("Incomplete journal segment cannot be archived"));
    }
    let pending = directory.join(format!("{index:016x}.output.gz.pending"));
    let mut encoder = GzEncoder::new(private_file(&pending)?, Compression::fast());
    if io::copy(&mut source.take(segment_bytes + 1), &mut encoder)? != segment_bytes {
        return Err(invalid("Journal segment changed while archiving"));
    }
    let file = encoder.finish()?;
    file.sync_all()?;
    let stored = file.metadata()?.len();
    fs::rename(pending, archive_path(directory, index))?;
    File::open(directory)?.sync_all()?;
    Ok(stored)
}

fn read_archive(directory: &Path, index: u64, segment_bytes: u64) -> io::Result<Vec<u8>> {
    let file = File::open(archive_path(directory, index))?;
    if file.metadata()?.len() > segment_bytes * 2 + 512 {
        return Err(invalid("Oversized compressed journal segment"));
    }
    let mut bytes = Vec::with_capacity(segment_bytes as usize);
    GzDecoder::new(file).take(segment_bytes + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != segment_bytes {
        return Err(invalid("Invalid compressed journal segment length"));
    }
    Ok(bytes)
}

pub fn retained_storage_bytes(directory: &Path) -> io::Result<u64> {
    let snapshot = manifest(directory)?;
    snapshot.archived_bytes.checked_add(snapshot.end - snapshot.first)
        .ok_or_else(|| invalid("Journal storage size overflow"))
}

/// The caller must first verify that the owning tmux pane has exited.
pub fn request_finalization(directory: &Path) -> io::Result<()> {
    match private_file(&directory.join("finalize.request")) {
        Ok(file) => {
            file.sync_all()?;
            File::open(directory)?.sync_all()
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            finalization_requested(directory).map(|_| ())
        }
        Err(error) => Err(error),
    }
}

fn finalization_requested(directory: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(directory.join("finalize.request")) {
        Ok(metadata) if metadata.is_file() && metadata.permissions().mode() & 0o077 == 0 => Ok(true),
        Ok(_) => Err(invalid("Invalid recorder finalization request")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn drain_input(recorder: &mut Recorder, input: &mut impl Read) -> io::Result<()> {
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = match input.read(&mut buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                // The request is issued only after tmux has drained the dead
                // pane. Consume every pending socket byte before acknowledging it.
                if finalization_requested(&recorder.directory)? { return Ok(()); }
                continue;
            }
            result => result?,
        };
        if count == 0 { return Ok(()); }
        recorder.append(&buffer[..count])?;
    }
}

impl Recorder {
    /// Creation is exclusive. Never restart a recorder over an old journal:
    /// output emitted while it was down cannot be reconstructed faithfully.
    pub fn create(directory: &Path) -> io::Result<Self> {
        Self::create_with_policy(directory, RetentionPolicy::default())
    }

    pub fn create_with_policy(directory: &Path, policy: RetentionPolicy) -> io::Result<Self> {
        Self::with_configuration(directory, SEGMENT_BYTES, RETAINED_SEGMENTS,
            policy == RetentionPolicy::UntilWorkspaceDeleted)
    }

    #[cfg(test)]
    fn with_retention(directory: &Path, segment_bytes: u64, retained: u64) -> io::Result<Self> {
        Self::with_configuration(directory, segment_bytes, retained, false)
    }

    fn with_configuration(directory: &Path, segment_bytes: u64, retained: u64, archive: bool) -> io::Result<Self> {
        if segment_bytes == 0
            || segment_bytes > SEGMENT_BYTES
            || retained == 0
            || retained > RETAINED_SEGMENTS
        {
            return Err(invalid("Invalid journal retention configuration"));
        }
        fs::DirBuilder::new().mode(0o700).create(directory)?;
        let recorder = Self {
            directory: directory.to_owned(),
            manifest: Manifest {
                segment_bytes,
                retained_segments: retained,
                first: 0,
                end: 0,
                closed: false,
                archive,
                archived_bytes: 0,
            },
            segment: None,
        };
        recorder.publish()?;
        Ok(recorder)
    }

    pub fn append(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        if self.manifest.closed {
            return Err(invalid("Cannot append after journal EOF"));
        }
        // Publish each segment before rotating. A large write cannot temporarily
        // evade retention or leave readers referring to an already deleted file.
        while !bytes.is_empty() {
            let size = self.manifest.segment_bytes;
            let index = self.manifest.end / size;
            let offset = self.manifest.end % size;
            if offset == 0 {
                self.segment = Some(private_file(&segment_path(&self.directory, index))?);
            }
            let count = bytes.len().min((size - offset) as usize);
            let end = self
                .manifest
                .end
                .checked_add(count as u64)
                .ok_or_else(|| invalid("Journal cursor overflow"))?;
            let file = self
                .segment
                .as_mut()
                .ok_or_else(|| invalid("Missing journal segment"))?;
            file.write_all(&bytes[..count])?;
            file.sync_data()?;
            let previous_first = self.manifest.first;
            let first = index.saturating_sub(self.manifest.retained_segments - 1) * size;
            let mut archived_bytes = self.manifest.archived_bytes;
            if self.manifest.archive {
                for old in (previous_first / size)..(first / size) {
                    archived_bytes = archived_bytes.checked_add(archive_segment(&self.directory, old, size)?)
                        .ok_or_else(|| invalid("Journal storage size overflow"))?;
                }
            }
            self.manifest.end = end;
            self.manifest.first = first;
            self.manifest.archived_bytes = archived_bytes;
            self.publish()?;
            for old in (previous_first / size)..(self.manifest.first / size) {
                fs::remove_file(segment_path(&self.directory, old))?;
            }
            bytes = &bytes[count..];
        }
        Ok(())
    }

    pub fn close(mut self) -> io::Result<()> {
        self.manifest.closed = true;
        self.publish()
    }

    fn publish(&self) -> io::Result<()> {
        let pending = self.directory.join("manifest.pending");
        let mut file = private_file(&pending)?;
        write!(
            file,
            "{} {} {} {} {} {}",
            if self.manifest.archive { "EWJ2" } else { "EWJ1" },
            self.manifest.segment_bytes,
            self.manifest.retained_segments,
            self.manifest.first,
            self.manifest.end,
            u8::from(self.manifest.closed)
        )?;
        if self.manifest.archive { write!(file, " {}", self.manifest.archived_bytes)?; }
        writeln!(file)?;
        file.sync_all()?;
        fs::rename(pending, self.directory.join("manifest"))?;
        // Rename durability matters: after a power failure it is preferable to
        // retain an unpublished tail rather than advertise missing output.
        File::open(&self.directory)?.sync_all()
    }
}

// The dependency-free Linux VM runner also compiles this module with Rust 1.75.
#[allow(clippy::manual_is_multiple_of)]
fn manifest(directory: &Path) -> io::Result<Manifest> {
    let mut contents = String::new();
    File::open(directory.join("manifest"))?
        .take(257)
        .read_to_string(&mut contents)?;
    if contents.len() > 256 {
        return Err(invalid("Oversized journal manifest"));
    }
    let fields: Vec<_> = contents.split_whitespace().collect();
    let archive = match fields.first().copied() {
        Some("EWJ1") if fields.len() == 6 => false,
        Some("EWJ2") if fields.len() == 7 => true,
        _ => return Err(invalid("Invalid journal manifest")),
    };
    if fields.len() > 7 {
        return Err(invalid("Invalid journal manifest"));
    }
    let number = |index: usize| {
        fields[index]
            .parse::<u64>()
            .map_err(|_| invalid("Invalid journal cursor"))
    };
    let result = Manifest {
        segment_bytes: number(1)?,
        retained_segments: number(2)?,
        first: number(3)?,
        end: number(4)?,
        closed: match fields[5] {
            "0" => false,
            "1" => true,
            _ => return Err(invalid("Invalid journal EOF")),
        },
        archive,
        archived_bytes: if archive { number(6)? } else { 0 },
    };
    if result.segment_bytes == 0
        || result.segment_bytes > SEGMENT_BYTES
        || result.retained_segments == 0
        || result.retained_segments > RETAINED_SEGMENTS
        || result.first > result.end
        || result.first % result.segment_bytes != 0
        || result.end - result.first > result.segment_bytes * result.retained_segments
    {
        return Err(invalid("Inconsistent journal manifest"));
    }
    Ok(result)
}

/// Read from one immutable published high-water mark. Polling again at
/// `next_cursor` is the same operation for historical and newly arriving output;
/// there is no separate subscribe step in which bytes can fall through a gap.
pub fn read(directory: &Path, cursor: u64, limit: usize) -> io::Result<OutputPage> {
    if limit == 0 || limit > MAX_PAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid output page size",
        ));
    }
    // Rotation can remove a segment between reading the manifest and opening
    // it. Retry against a fresh manifest, reporting the resulting retention gap.
    for _ in 0..3 {
        let snapshot = manifest(directory)?;
        if cursor > snapshot.end {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Cursor is ahead of this journal",
            ));
        }
        let earliest = if snapshot.archive { 0 } else { snapshot.first };
        let start = cursor.max(earliest);
        let length = (snapshot.end - start).min(limit as u64) as usize;
        let mut bytes = vec![0; length];
        let mut copied = 0;
        let mut raced = false;
        while copied < length {
            let position = start + copied as u64;
            let index = position / snapshot.segment_bytes;
            let offset = position % snapshot.segment_bytes;
            let count = (length - copied).min((snapshot.segment_bytes - offset) as usize);
            let mut file = match File::open(segment_path(directory, index)) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound && snapshot.archive => {
                    match read_archive(directory, index, snapshot.segment_bytes) {
                        Ok(archived) => {
                            bytes[copied..copied + count]
                                .copy_from_slice(&archived[offset as usize..offset as usize + count]);
                            copied += count;
                            continue;
                        }
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {
                            raced = true;
                            break;
                        }
                        Err(error) => return Err(error),
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    raced = true;
                    break;
                }
                Err(error) => return Err(error),
            };
            file.seek(SeekFrom::Start(offset))?;
            file.read_exact(&mut bytes[copied..copied + count])?;
            copied += count;
        }
        if raced {
            continue;
        }
        return Ok(OutputPage {
            start_cursor: start,
            next_cursor: start + length as u64,
            earliest_cursor: earliest,
            high_watermark: snapshot.end,
            history_gap: cursor < earliest,
            closed: snapshot.closed,
            bytes,
        });
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "Output rotated while reading; retry the same cursor",
    ))
}

/// Recognized before GUI initialization in the extension executable. No
/// listening socket, SSH connection, application window or daemon is created.
pub fn run_recorder_if_requested() -> Option<io::Result<()>> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new(RECORDER_ARGUMENT)) {
        return None;
    }
    Some((|| {
        let directory = PathBuf::from(args.next().ok_or_else(|| invalid("Missing journal path"))?);
        let policy = match args.next().as_deref() {
            None => RetentionPolicy::UntilWorkspaceDeleted,
            Some(value) if value == std::ffi::OsStr::new("--bounded-history") => RetentionPolicy::Rolling,
            Some(_) => return Err(invalid("Unknown journal retention policy")),
        };
        if args.next().is_some() || !directory.is_absolute() {
            return Err(invalid("Expected an absolute journal path and optional retention policy"));
        }
        let mut recorder = Recorder::create_with_policy(&directory, policy)?;
        let stdin = io::stdin();
        // tmux pipe-pane supplies a Unix socket. Ordinary redirected stdin is
        // also supported, with clean EOF as its completion signal.
        let socket = UnixStream::from(stdin.as_fd().try_clone_to_owned()?);
        let mut input: Box<dyn Read> = if socket.set_read_timeout(Some(Duration::from_millis(100))).is_ok() {
            Box::new(socket)
        } else {
            Box::new(stdin.lock())
        };
        drain_input(&mut recorder, &mut input)?;
        recorder.close()
    })())
}

#[cfg(test)]
#[path = "persistent_journal_tests.rs"]
mod tests;
