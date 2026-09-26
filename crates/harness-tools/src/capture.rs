//! Bounded process-output capture (M4-03.4).
//!
//! A tool process may write a log far larger than anything the host should hold
//! in memory, and a model still has to be able to read the part it needs. The
//! capture therefore streams each output stream straight to a spool file,
//! keeping only a bounded head preview and a bounded tail preview in memory, and
//! publishes the spooled bytes as a durable artifact the model can page through
//! with `read_process_output`.
//!
//! Two invariants live here:
//!
//!   * **Bounded memory.** Nothing in this module grows with the size of the
//!     log: reads are chunked, previews are capped, and a stream that exceeds
//!     its quota is cut at the quota and marked truncated rather than buffered.
//!   * **No secret ever lands on disk.** Granted values are redacted while the
//!     bytes are written, not afterwards, so a raw credential never exists in
//!     the spool file, the published artifact, or a preview.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use harness_types::{ContentHash, ErrorCode, HarnessError};

/// Serialization revision of the capture header that precedes the raw bytes.
pub const CAPTURE_HEADER_VERSION: u16 = 1;

/// Largest capture the host keeps per stream unless a deployment says otherwise.
pub const DEFAULT_CAPTURE_QUOTA_BYTES: u64 = 1024 * 1024;

/// Largest head preview handed to the model for one stream.
pub const DEFAULT_HEAD_PREVIEW_BYTES: usize = 64 * 1024;

/// Largest tail preview handed to the model for the whole capture.
pub const DEFAULT_TAIL_PREVIEW_BYTES: usize = 4 * 1024;

/// The header line every process capture artifact starts with. It is host
/// framing, not model content: it lets a paged read find a stream's bytes
/// without re-running anything.
#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize)]
pub struct CaptureHeader {
    pub schema_version: u16,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub quota_bytes: u64,
}

impl CaptureHeader {
    /// Byte offset of the stdout section inside the artifact.
    #[must_use]
    pub fn stdout_offset(&self) -> u64 {
        self.header_len()
    }

    /// Byte offset of the stderr section inside the artifact.
    #[must_use]
    pub fn stderr_offset(&self) -> u64 {
        self.header_len() + self.stdout_bytes
    }

    #[must_use]
    fn header_len(&self) -> u64 {
        header_line(self).len() as u64
    }
}

/// Parse the header line of a capture artifact.
///
/// A well-formed artifact always carries one; a JSON body that does not is a
/// different kind of artifact, and saying so is better than reading it as a log.
pub fn parse_capture_header(bytes: &[u8]) -> Result<CaptureHeader, HarnessError> {
    let newline = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(|| {
            HarnessError::new(
                ErrorCode::InvalidPayload,
                "artifact does not start with a process capture header",
            )
        })?;
    let header: CaptureHeader = serde_json::from_slice(&bytes[..newline]).map_err(|_| {
        HarnessError::new(
            ErrorCode::InvalidPayload,
            "artifact does not start with a process capture header",
        )
    })?;
    if header.schema_version != CAPTURE_HEADER_VERSION {
        return Err(HarnessError::new(
            ErrorCode::UnsupportedSchemaVersion,
            format!(
                "capture header schema {} is not supported",
                header.schema_version
            ),
        ));
    }
    Ok(header)
}

fn header_line(header: &CaptureHeader) -> String {
    // The header is written by this host only, so serialization cannot fail; a
    // fallback keeps the signature total without inventing a success value.
    serde_json::to_string(header).unwrap_or_else(|_| {
        format!(
            "{{\"schema_version\":{CAPTURE_HEADER_VERSION},\"stdout_bytes\":0,\"stderr_bytes\":0,\"stdout_truncated\":false,\"stderr_truncated\":false,\"quota_bytes\":0}}"
        )
    }) + "\n"
}

/// How much the host captures and previews.
#[derive(Clone, Copy, Debug)]
pub struct SpoolLimits {
    /// Bytes kept per stream. A stream that exceeds this is cut and marked
    /// truncated; the rest of it is drained and discarded.
    pub max_capture_bytes: u64,
    pub head_preview_bytes: usize,
    pub tail_preview_bytes: usize,
}

impl Default for SpoolLimits {
    fn default() -> Self {
        Self {
            max_capture_bytes: DEFAULT_CAPTURE_QUOTA_BYTES,
            head_preview_bytes: DEFAULT_HEAD_PREVIEW_BYTES,
            tail_preview_bytes: DEFAULT_TAIL_PREVIEW_BYTES,
        }
    }
}

/// Where captures are spooled before they are published.
///
/// The default is the OS temporary directory because a spool file is staging,
/// not evidence: the durable copy is the published artifact. A deployment may
/// point it at a specific volume, and a test points it somewhere unwritable to
/// prove a failed capture is reported instead of faked.
#[derive(Clone, Debug)]
pub struct ProcessSpoolConfig {
    root: PathBuf,
    limits: SpoolLimits,
}

impl Default for ProcessSpoolConfig {
    fn default() -> Self {
        Self {
            root: std::env::temp_dir().join("ha-tool-spool"),
            limits: SpoolLimits::default(),
        }
    }
}

impl ProcessSpoolConfig {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>, limits: SpoolLimits) -> Self {
        Self {
            root: root.into(),
            limits,
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub const fn limits(&self) -> SpoolLimits {
        self.limits
    }
}

/// A streaming redactor for granted secret values.
///
/// It holds back only as many bytes as the longest secret, so a value split
/// across two reads is still replaced, and the memory it uses is bounded by the
/// secrets themselves rather than by the log.
pub(crate) struct Redactor {
    secrets: Vec<Vec<u8>>,
    carry: Vec<u8>,
    overlap: usize,
}

impl Redactor {
    #[must_use]
    pub(crate) fn new(secrets: &[Vec<u8>]) -> Self {
        let overlap = secrets
            .iter()
            .map(Vec::len)
            .max()
            .unwrap_or(0)
            .saturating_sub(1);
        Self {
            secrets: secrets.to_vec(),
            carry: Vec::new(),
            overlap,
        }
    }

    /// Redact everything that can no longer be part of a split secret.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        if self.secrets.is_empty() {
            return chunk.to_vec();
        }
        let mut window = std::mem::take(&mut self.carry);
        window.extend_from_slice(chunk);
        let limit = window.len().saturating_sub(self.overlap);
        let mut out = Vec::with_capacity(window.len());
        let mut index = 0;
        while index < window.len() {
            if let Some(secret) = self
                .secrets
                .iter()
                .find(|secret| !secret.is_empty() && window[index..].starts_with(secret))
            {
                out.extend_from_slice(b"[REDACTED]");
                index += secret.len();
                continue;
            }
            if index >= limit {
                break;
            }
            out.push(window[index]);
            index += 1;
        }
        self.carry = window[index..].to_vec();
        out
    }

    /// Flush the held-back bytes at end of stream.
    pub(crate) fn finish(mut self) -> Vec<u8> {
        if self.secrets.is_empty() {
            return std::mem::take(&mut self.carry);
        }
        let window = std::mem::take(&mut self.carry);
        let mut out = Vec::with_capacity(window.len());
        let mut index = 0;
        while index < window.len() {
            if let Some(secret) = self
                .secrets
                .iter()
                .find(|secret| !secret.is_empty() && window[index..].starts_with(secret))
            {
                out.extend_from_slice(b"[REDACTED]");
                index += secret.len();
                continue;
            }
            out.push(window[index]);
            index += 1;
        }
        out
    }
}

/// The end of one output stream, as the model is shown it.
///
/// A command log ends with what matters — the error, the summary, the exit
/// status line — so the model reads its tail (prime-agent's output
/// accumulator). The tail is kept from everything the stream produced, past
/// the capture quota too: the quota bounds what is stored, not what the model
/// may see of the end. The totals let the reader say which lines it shows.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize)]
pub struct StreamTail {
    /// The last bytes of the stream, starting on a line boundary whenever the
    /// kept window allows it. Bounded by [`STREAM_TAIL_BYTES`].
    pub text: String,
    /// Bytes the stream produced.
    pub total_bytes: u64,
    /// Lines the stream produced, counted as `split('\n')` counts them: an
    /// empty stream is one empty line.
    pub total_lines: u64,
    /// Bytes of the last line, which may be longer than `text` holds.
    pub last_line_bytes: u64,
}

/// Bytes of a stream's end kept in memory: twice the tool-output byte limit,
/// so a tail cut always has whole lines to choose from (prime's rolling
/// window). The window is trimmed once it doubles, so memory stays bounded.
pub const STREAM_TAIL_BYTES: usize = 2 * crate::truncate::DEFAULT_MAX_BYTES;

/// The rolling end of a stream while it is being written.
#[derive(Debug)]
struct TailWindow {
    bytes: Vec<u8>,
    starts_at_line_boundary: bool,
    total_bytes: u64,
    total_lines: u64,
    last_line_bytes: u64,
}

impl Default for TailWindow {
    fn default() -> Self {
        Self {
            bytes: Vec::new(),
            starts_at_line_boundary: true,
            total_bytes: 0,
            total_lines: 1,
            last_line_bytes: 0,
        }
    }
}

impl TailWindow {
    fn push(&mut self, chunk: &[u8]) {
        self.total_bytes += chunk.len() as u64;
        match chunk.iter().rposition(|byte| *byte == b'\n') {
            Some(last) => {
                #[allow(
                    clippy::naive_bytecount,
                    reason = "one count per 8 KiB read; not worth a dependency"
                )]
                let newlines = chunk.iter().filter(|byte| **byte == b'\n').count();
                self.total_lines += newlines as u64;
                self.last_line_bytes = (chunk.len() - last - 1) as u64;
            }
            None => self.last_line_bytes += chunk.len() as u64,
        }
        self.bytes.extend_from_slice(chunk);
        if self.bytes.len() > STREAM_TAIL_BYTES * 2 {
            self.trim();
        }
    }

    /// Keep the last [`STREAM_TAIL_BYTES`], starting on a character boundary.
    fn trim(&mut self) {
        if self.bytes.len() <= STREAM_TAIL_BYTES {
            return;
        }
        let mut start = self.bytes.len() - STREAM_TAIL_BYTES;
        // Skip UTF-8 continuation bytes so the window never starts mid-character.
        while start < self.bytes.len() && self.bytes[start] & 0xc0 == 0x80 {
            start += 1;
        }
        self.starts_at_line_boundary = self.bytes[start - 1] == b'\n';
        self.bytes.drain(..start);
    }

    fn finish(mut self) -> StreamTail {
        self.trim();
        let text = String::from_utf8_lossy(&self.bytes).into_owned();
        // A window that starts inside a line drops that partial line.
        let text = match (self.starts_at_line_boundary, text.find('\n')) {
            (false, Some(newline)) => text[newline + 1..].to_owned(),
            _ => text,
        };
        StreamTail {
            text,
            total_bytes: self.total_bytes,
            total_lines: self.total_lines,
            last_line_bytes: self.last_line_bytes,
        }
    }
}

/// One stream being spooled.
#[derive(Debug)]
pub(crate) struct SpoolWriter {
    file: File,
    path: PathBuf,
    captured: u64,
    /// Everything the stream produced, including the bytes the quota dropped.
    seen: u64,
    quota: u64,
    truncated: bool,
    head: Vec<u8>,
    head_limit: usize,
    tail: TailWindow,
}

impl SpoolWriter {
    pub(crate) fn create(
        root: &Path,
        limits: SpoolLimits,
        label: &str,
    ) -> Result<Self, HarnessError> {
        std::fs::create_dir_all(root)
            .map_err(|error| spool_error("create the spool directory", &error))?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let path = root.join(format!("{label}-{}-{stamp}.capture", std::process::id()));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| spool_error("create a spool file", &error))?;
        Ok(Self {
            file,
            path,
            captured: 0,
            seen: 0,
            quota: limits.max_capture_bytes,
            truncated: false,
            head: Vec::new(),
            head_limit: limits.head_preview_bytes,
            tail: TailWindow::default(),
        })
    }

    /// Write one already-redacted chunk, keeping only the head preview and the
    /// rolling tail in memory.
    pub(crate) fn write(&mut self, chunk: &[u8]) -> Result<(), HarnessError> {
        self.seen += chunk.len() as u64;
        self.tail.push(chunk);
        if self.head.len() < self.head_limit {
            let room = self.head_limit - self.head.len();
            self.head.extend_from_slice(&chunk[..room.min(chunk.len())]);
        }
        let room = self.quota.saturating_sub(self.captured);
        let take = usize::try_from(room.min(chunk.len() as u64)).unwrap_or(chunk.len());
        if take < chunk.len() {
            self.truncated = true;
        }
        if take > 0 {
            self.file
                .write_all(&chunk[..take])
                .map_err(|error| spool_error("write a spool file", &error))?;
            self.captured += take as u64;
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> SpooledStream {
        // The spool file is staging: it is read back immediately and then
        // published through the store, which is what flushes and fsyncs the
        // durable copy. Syncing here would only add latency per process call.
        SpooledStream {
            path: self.path,
            captured: self.captured,
            seen: self.seen,
            truncated: self.truncated,
            head: String::from_utf8_lossy(&self.head).into_owned(),
            tail: self.tail.finish(),
        }
    }
}

/// One finished stream: its bytes are on disk, its previews are in memory.
#[derive(Debug)]
pub(crate) struct SpooledStream {
    pub(crate) path: PathBuf,
    pub(crate) captured: u64,
    /// Bytes the stream produced, captured or not.
    pub(crate) seen: u64,
    pub(crate) truncated: bool,
    pub(crate) head: String,
    pub(crate) tail: StreamTail,
}

/// The published shape of a capture: bytes on disk, previews in memory.
#[derive(Clone, Debug)]
pub(crate) struct FinalizedCapture {
    pub(crate) path: PathBuf,
    pub(crate) bytes: u64,
    pub(crate) hash: ContentHash,
    pub(crate) truncated: bool,
    pub(crate) stdout_head: String,
    pub(crate) stderr_head: String,
    pub(crate) tail: String,
    /// Whether each preview is shorter than the stream it previews. This is the
    /// model-facing meaning of "truncated"; the header written into the artifact
    /// says whether the *capture* stopped at the quota, which is a different
    /// claim.
    pub(crate) stdout_preview_truncated: bool,
    pub(crate) stderr_preview_truncated: bool,
    pub(crate) stdout_tail: StreamTail,
    pub(crate) stderr_tail: StreamTail,
}

/// Assemble `header + stdout + stderr` into the artifact payload.
///
/// Both streams are copied through the redactor in chunks, so the only memory
/// this uses is the copy buffer plus the bounded previews.
pub(crate) fn finalize_capture(
    stdout: SpooledStream,
    stderr: SpooledStream,
    limits: SpoolLimits,
) -> Result<FinalizedCapture, HarnessError> {
    let header = CaptureHeader {
        schema_version: CAPTURE_HEADER_VERSION,
        stdout_bytes: stdout.captured,
        stderr_bytes: stderr.captured,
        stdout_truncated: stdout.truncated,
        stderr_truncated: stderr.truncated,
        quota_bytes: limits.max_capture_bytes,
    };
    let line = header_line(&header);
    let path = stdout.path.with_extension("artifact");
    let mut payload =
        File::create(&path).map_err(|error| spool_error("create a capture", &error))?;
    payload
        .write_all(line.as_bytes())
        .map_err(|error| spool_error("write a capture header", &error))?;
    copy_into(&stdout.path, &mut payload)?;
    copy_into(&stderr.path, &mut payload)?;
    payload
        .flush()
        .map_err(|error| spool_error("flush a capture", &error))?;
    drop(payload);
    let bytes = std::fs::metadata(&path)
        .map_err(|error| spool_error("measure a capture", &error))?
        .len();
    // Hash exactly the bytes that will be published, streaming, so the digest a
    // receipt carries is the digest of the artifact a reader will get back.
    let mut reader = File::open(&path).map_err(|error| spool_error("read a capture", &error))?;
    let hash = ContentHash::from_reader(&mut reader)
        .map_err(|error| spool_error("hash a capture", &error))?;
    let tail = read_tail(&path, limits.tail_preview_bytes)?;
    let stdout_preview_truncated = stdout.seen > stdout.head.len() as u64;
    let stderr_preview_truncated = stderr.seen > stderr.head.len() as u64;
    let _ = std::fs::remove_file(&stdout.path);
    let _ = std::fs::remove_file(&stderr.path);
    Ok(FinalizedCapture {
        path,
        bytes,
        hash,
        truncated: stdout.truncated || stderr.truncated,
        stdout_head: stdout.head,
        stderr_head: stderr.head,
        tail,
        stdout_preview_truncated,
        stderr_preview_truncated,
        stdout_tail: stdout.tail,
        stderr_tail: stderr.tail,
    })
}

fn copy_into(source: &Path, payload: &mut File) -> Result<(), HarnessError> {
    let mut input = File::open(source).map_err(|error| spool_error("read a spool file", &error))?;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = input
            .read(&mut buffer)
            .map_err(|error| spool_error("read a spool file", &error))?;
        if read == 0 {
            break;
        }
        payload
            .write_all(&buffer[..read])
            .map_err(|error| spool_error("write a capture", &error))?;
    }
    Ok(())
}

/// The last `limit` bytes of a file, as lossy text.
pub(crate) fn read_tail(path: &Path, limit: usize) -> Result<String, HarnessError> {
    let mut file = File::open(path).map_err(|error| spool_error("read a capture", &error))?;
    let length = file
        .metadata()
        .map_err(|error| spool_error("measure a capture", &error))?
        .len();
    let start = length.saturating_sub(limit as u64);
    file.seek(SeekFrom::Start(start))
        .map_err(|error| spool_error("seek a capture", &error))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| spool_error("read a capture tail", &error))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn spool_error(action: &str, error: &std::io::Error) -> HarnessError {
    HarnessError::new(
        ErrorCode::ArtifactWriteFailed,
        format!("cannot {action}: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tail_of(chunks: &[&[u8]]) -> StreamTail {
        let mut window = TailWindow::default();
        for chunk in chunks {
            window.push(chunk);
        }
        window.finish()
    }

    #[test]
    fn a_short_stream_keeps_all_of_it_and_counts_lines_as_split_does() {
        let tail = tail_of(&[b"one\ntw", b"o\nthree"]);
        assert_eq!(tail.text, "one\ntwo\nthree");
        assert_eq!(tail.total_lines, 3);
        assert_eq!(tail.total_bytes, 13);
        assert_eq!(tail.last_line_bytes, 5);

        let empty = tail_of(&[]);
        assert_eq!((empty.text.as_str(), empty.total_lines), ("", 1));
    }

    #[test]
    fn a_long_stream_keeps_only_its_end_starting_at_a_whole_line() {
        let line = format!("{}\n", "x".repeat(99));
        let chunk = line.repeat(1000);
        let tail = tail_of(&[
            chunk.as_bytes(),
            chunk.as_bytes(),
            chunk.as_bytes(),
            b"last",
        ]);
        assert!(tail.text.len() <= STREAM_TAIL_BYTES, "{}", tail.text.len());
        assert!(tail.text.starts_with('x'));
        assert!(tail.text.ends_with("\nlast"));
        // Every kept line is whole: the partial first line of the window went.
        assert!(
            tail.text
                .split('\n')
                .rev()
                .skip(1)
                .all(|kept| kept.len() == 99)
        );
        assert_eq!(tail.total_lines, 3001);
        assert_eq!(tail.total_bytes, 3 * chunk.len() as u64 + 4);
    }

    #[test]
    fn the_window_never_starts_inside_a_character() {
        // Three-byte characters: the cut point 2 * STREAM_TAIL_BYTES is not a
        // multiple of three, so it lands inside one.
        let text = "€".repeat(STREAM_TAIL_BYTES);
        let tail = tail_of(&[text.as_bytes()]);
        assert!(!tail.text.contains('\u{fffd}'));
        assert!(tail.text.chars().all(|character| character == '€'));
        assert_eq!(tail.last_line_bytes, text.len() as u64);
    }
}
