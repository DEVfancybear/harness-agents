//! Bounded process-output capture (M4-03.4).
//!
//! A tool process may write a log far larger than anything the host should hold
//! in memory, and a model still has to be able to read the part it needs. The
//! capture therefore streams each output stream to a spool file once it
//! outgrows a small in-memory buffer, keeping only a bounded head preview and a
//! bounded tail preview in memory, and
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
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use harness_types::{ErrorCode, HarnessError};

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

/// Captured bytes a stream keeps in memory before it spills to a spool file.
///
/// Most tool processes - `git status`, a hook, a guard probe, a short build
/// step - write a few kilobytes. Creating, writing, reopening and deleting a
/// file for each of their streams costs more on a Windows disk with antivirus
/// scanning than the process itself, so a stream only gets a file once it has
/// outgrown this. The bound keeps memory per stream fixed: a stream past it
/// holds nothing but the previews.
pub(crate) const SPILL_THRESHOLD_BYTES: usize = 64 * 1024;

/// One stream being spooled.
#[derive(Debug)]
pub(crate) struct SpoolWriter {
    /// The spool file, once the stream outgrew [`SPILL_THRESHOLD_BYTES`].
    file: Option<(File, SpoolFile)>,
    /// The captured bytes while the stream is still small enough to keep.
    memory: Vec<u8>,
    root: PathBuf,
    label: String,
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
        // The directory is still created up front, although the file may never
        // be: a spool location that cannot be written is a misconfiguration the
        // call reports, whether or not this particular process wrote enough to
        // need it.
        std::fs::create_dir_all(root)
            .map_err(|error| spool_error("create the spool directory", &error))?;
        sweep_stale_spool_files(root);
        Ok(Self {
            file: None,
            memory: Vec::new(),
            root: root.to_owned(),
            label: label.to_owned(),
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
    /// rolling tail in memory once the stream has spilled.
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
        if take == 0 {
            return Ok(());
        }
        if self.file.is_none() && self.memory.len() + take > SPILL_THRESHOLD_BYTES {
            self.spill()?;
        }
        match &mut self.file {
            Some((file, _)) => file
                .write_all(&chunk[..take])
                .map_err(|error| spool_error("write a spool file", &error))?,
            None => self.memory.extend_from_slice(&chunk[..take]),
        }
        self.captured += take as u64;
        Ok(())
    }

    /// Move the bytes kept so far into a spool file, which takes every later
    /// write.
    fn spill(&mut self) -> Result<(), HarnessError> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let path = self.root.join(format!(
            "{}-{}-{stamp}.capture",
            self.label,
            std::process::id()
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| spool_error("create a spool file", &error))?;
        // From here on the file is owned by its guard, so a failed write below
        // still removes it.
        let guard = SpoolFile { path };
        file.write_all(&self.memory)
            .map_err(|error| spool_error("write a spool file", &error))?;
        self.memory = Vec::new();
        self.file = Some((file, guard));
        Ok(())
    }

    pub(crate) fn finish(self) -> SpooledStream {
        // The spool file is staging: it is read back once and then published
        // through the store, which is what flushes and fsyncs the durable copy.
        // Syncing here would only add latency per process call.
        let body = match self.file {
            Some((file, guard)) => {
                drop(file);
                SpoolBody::File(guard)
            }
            None => SpoolBody::Memory(self.memory),
        };
        SpooledStream {
            body,
            captured: self.captured,
            seen: self.seen,
            truncated: self.truncated,
            head: String::from_utf8_lossy(&self.head).into_owned(),
            tail: self.tail.finish(),
        }
    }
}

/// A spool file that is removed when the last owner lets go of it.
///
/// Every caller of the process runner gets a capture, and most of them - the
/// git tools, hooks, guard probes, quality gates - only read its previews and
/// never publish it. Tying the file's life to a value instead of to the one
/// caller that published it is what keeps the spool directory from filling up
/// with files nobody will read again.
#[derive(Debug)]
pub(crate) struct SpoolFile {
    path: PathBuf,
}

impl Drop for SpoolFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Where one finished stream's captured bytes are.
#[derive(Debug)]
pub(crate) enum SpoolBody {
    Memory(Vec<u8>),
    File(SpoolFile),
}

impl SpoolBody {
    /// Append the captured bytes to `payload`.
    fn append_to(&self, payload: &mut Vec<u8>) -> Result<(), HarnessError> {
        match self {
            Self::Memory(bytes) => {
                payload.extend_from_slice(bytes);
                Ok(())
            }
            Self::File(file) => File::open(&file.path)
                .and_then(|mut input| input.read_to_end(payload))
                .map(|_| ())
                .map_err(|error| spool_error("read a spool file", &error)),
        }
    }
}

/// One finished stream: its captured bytes in memory or on disk, its previews
/// in memory.
#[derive(Debug)]
pub(crate) struct SpooledStream {
    pub(crate) body: SpoolBody,
    pub(crate) captured: u64,
    /// Bytes the stream produced, captured or not.
    pub(crate) seen: u64,
    pub(crate) truncated: bool,
    pub(crate) head: String,
    pub(crate) tail: StreamTail,
}

/// The finished shape of a capture: what the artifact will hold, and the
/// previews in memory.
///
/// The artifact bytes are only assembled when a caller publishes them
/// ([`FinalizedCapture::payload`]): the callers that never publish do not pay
/// for a copy, a hash, or a file of their own. The streams' storage is shared,
/// so a clone of a result does not remove a spool file another clone still
/// needs.
#[derive(Clone, Debug)]
pub(crate) struct FinalizedCapture {
    header: CaptureHeader,
    streams: std::sync::Arc<(SpoolBody, SpoolBody)>,
    /// Length of the artifact [`Self::payload`] assembles.
    pub(crate) bytes: u64,
    pub(crate) truncated: bool,
    tail_preview_bytes: usize,
    pub(crate) stdout_head: String,
    pub(crate) stderr_head: String,
    /// Whether each preview is shorter than the stream it previews. This is the
    /// model-facing meaning of "truncated"; the header written into the artifact
    /// says whether the *capture* stopped at the quota, which is a different
    /// claim.
    pub(crate) stdout_preview_truncated: bool,
    pub(crate) stderr_preview_truncated: bool,
    pub(crate) stdout_tail: StreamTail,
    pub(crate) stderr_tail: StreamTail,
}

impl FinalizedCapture {
    /// Assemble `header + stdout + stderr`: exactly the bytes to publish.
    ///
    /// The capture quota bounds both sections, so this is at most twice the
    /// quota plus one header line - the same bytes a publisher has to hand the
    /// store anyway, now read once instead of copied, re-read to hash, and
    /// re-read again to publish.
    pub(crate) fn payload(&self) -> Result<Vec<u8>, HarnessError> {
        let line = header_line(&self.header);
        let mut payload = Vec::with_capacity(usize::try_from(self.bytes).unwrap_or(line.len()));
        payload.extend_from_slice(line.as_bytes());
        self.streams.0.append_to(&mut payload)?;
        self.streams.1.append_to(&mut payload)?;
        if payload.len() as u64 != self.bytes {
            // A spool file changed under the host between the run and the
            // publish. Publishing it would make the receipt describe bytes the
            // header does not frame.
            return Err(HarnessError::new(
                ErrorCode::ArtifactWriteFailed,
                "the spooled capture changed before it was published",
            ));
        }
        Ok(payload)
    }

    /// The last bytes of an assembled payload, as lossy text: the tail preview
    /// a receipt carries, taken from the bytes a reader will get back.
    pub(crate) fn tail_of(&self, payload: &[u8]) -> String {
        let start = payload.len().saturating_sub(self.tail_preview_bytes);
        String::from_utf8_lossy(&payload[start..]).into_owned()
    }
}

/// Frame both streams as one capture.
///
/// Nothing is copied here: the bytes stay where the streams left them until a
/// caller publishes the capture.
pub(crate) fn finalize_capture(
    stdout: SpooledStream,
    stderr: SpooledStream,
    limits: SpoolLimits,
) -> FinalizedCapture {
    let header = CaptureHeader {
        schema_version: CAPTURE_HEADER_VERSION,
        stdout_bytes: stdout.captured,
        stderr_bytes: stderr.captured,
        stdout_truncated: stdout.truncated,
        stderr_truncated: stderr.truncated,
        quota_bytes: limits.max_capture_bytes,
    };
    let bytes = header.header_len() + stdout.captured + stderr.captured;
    FinalizedCapture {
        header,
        bytes,
        truncated: stdout.truncated || stderr.truncated,
        tail_preview_bytes: limits.tail_preview_bytes,
        stdout_preview_truncated: stdout.seen > stdout.head.len() as u64,
        stderr_preview_truncated: stderr.seen > stderr.head.len() as u64,
        stdout_head: stdout.head,
        stderr_head: stderr.head,
        stdout_tail: stdout.tail,
        stderr_tail: stderr.tail,
        streams: std::sync::Arc::new((stdout.body, stderr.body)),
    }
}

/// Remove what earlier hosts left in a spool directory, once per process.
///
/// Hosts before 0.2.4 copied every capture into a `.artifact` file that only
/// the published-process path removed, so a git tool, a hook or a guard probe
/// left one behind each run - thousands in a busy temp directory. Only those
/// files are swept, and only once they are an hour old: a `.capture` file may
/// belong to a long command another host is running right now, and an hour is
/// far past the moment an older host reads its `.artifact` back.
fn sweep_stale_spool_files(root: &Path) {
    static SWEPT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if SWEPT.set(()).is_err() {
        return;
    }
    let root = root.to_owned();
    // A directory with thousands of files takes a while to list; no call
    // waits for it.
    let _ = std::thread::Builder::new()
        .name("ha-spool-sweep".to_owned())
        .spawn(move || {
            let Ok(entries) = std::fs::read_dir(&root) else {
                return;
            };
            let cutoff = std::time::Duration::from_hours(1);
            for entry in entries.flatten() {
                let path = entry.path();
                if path
                    .extension()
                    .is_none_or(|extension| extension != "artifact")
                {
                    continue;
                }
                let stale = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .ok()
                    .and_then(|modified| modified.elapsed().ok())
                    .is_some_and(|age| age > cutoff);
                if stale {
                    let _ = std::fs::remove_file(&path);
                }
            }
        });
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

    fn spool_entries(root: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(root)
            .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
            .unwrap_or_default()
    }

    fn spooled(root: &Path, label: &str, chunks: &[&[u8]]) -> SpooledStream {
        let mut writer =
            SpoolWriter::create(root, SpoolLimits::default(), label).expect("spool writer");
        for chunk in chunks {
            writer.write(chunk).expect("spooled write");
        }
        writer.finish()
    }

    #[test]
    fn a_small_capture_never_touches_the_spool_directory() {
        let root = tempfile::tempdir().expect("spool root");
        let capture = finalize_capture(
            spooled(root.path(), "stdout", &[b"## main\n"]),
            spooled(root.path(), "stderr", &[]),
            SpoolLimits::default(),
        );
        assert!(spool_entries(root.path()).is_empty());
        let payload = capture.payload().expect("payload");
        assert_eq!(payload.len() as u64, capture.bytes);
        let header = parse_capture_header(&payload).expect("header");
        let start = usize::try_from(header.stdout_offset()).unwrap();
        assert_eq!(&payload[start..], b"## main\n");
    }

    #[test]
    fn a_large_capture_spills_and_its_file_goes_with_the_last_owner() {
        let root = tempfile::tempdir().expect("spool root");
        let big = vec![b'x'; SPILL_THRESHOLD_BYTES + 10];
        let capture = finalize_capture(
            spooled(root.path(), "stdout", &[b"head ", &big]),
            spooled(root.path(), "stderr", &[b"warning\n"]),
            SpoolLimits::default(),
        );
        assert_eq!(spool_entries(root.path()).len(), 1, "only stdout spilled");
        let payload = capture.payload().expect("payload");
        let header = parse_capture_header(&payload).expect("header");
        assert_eq!(header.stdout_bytes, 5 + big.len() as u64);
        let stderr = usize::try_from(header.stderr_offset()).unwrap();
        assert_eq!(&payload[stderr..], b"warning\n");
        assert_eq!(
            capture.tail_of(&payload),
            String::from_utf8_lossy(&payload[payload.len() - DEFAULT_TAIL_PREVIEW_BYTES..])
        );
        // A clone shares the file; only the last owner removes it. No `.artifact`
        // copy is ever made, whoever the caller is.
        let clone = capture.clone();
        drop(capture);
        assert_eq!(spool_entries(root.path()).len(), 1);
        drop(clone);
        assert!(
            spool_entries(root.path()).is_empty(),
            "{:?}",
            spool_entries(root.path())
        );
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
