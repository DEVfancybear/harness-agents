//! The kernel's stderr, as prime-agent's `repl-manager.ts` keeps it: an 8 KB tail
//! in memory for the errors the model and the user see, and an owner-only log
//! file on disk, capped at 5 MB and rotated once to `.old`.
//!
//! The runtime moves fd 2 into its protocol pump before it reports ready, so the
//! pipe carries what the interpreter printed while starting (import errors, a
//! broken venv) and whatever the host itself notes about the kernel.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use tokio::io::{AsyncRead, AsyncReadExt};

/// prime-agent's `MAX_KERNEL_STDERR_CHARS`.
pub const MAX_TAIL_CHARS: usize = 8 * 1024;
/// prime-agent's `MAX_KERNEL_STDERR_LOG_BYTES`.
pub const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
const BUDGET_MARKER: &str = "[stderr log budget exhausted]\n";
/// The log's name inside a conversation's kernel directory.
pub const LOG_FILE: &str = "kernel-stderr.log";

/// The last [`MAX_TAIL_CHARS`] characters the kernel wrote.
#[derive(Debug, Default)]
pub struct Tail {
    text: String,
}

impl Tail {
    pub fn push(&mut self, text: &str) {
        self.text.push_str(text);
        let count = self.text.chars().count();
        if count > MAX_TAIL_CHARS
            && let Some((cut, _)) = self.text.char_indices().nth(count - MAX_TAIL_CHARS)
        {
            self.text.drain(..cut);
        }
    }

    /// The last `chars` characters.
    #[must_use]
    pub fn last(&self, chars: usize) -> &str {
        let count = self.text.chars().count();
        if count <= chars {
            return &self.text;
        }
        let cut = self
            .text
            .char_indices()
            .nth(count - chars)
            .map_or(0, |(at, _)| at);
        &self.text[cut..]
    }
}

/// The on-disk log of one kernel. Its write budget is the file's remaining room,
/// not a fresh allowance, so the file and its `.old` each stay near the cap even
/// when rotation fails.
#[derive(Debug)]
pub struct Log {
    file: std::fs::File,
    budget: u64,
    writable: bool,
}

impl Log {
    /// Open (and rotate, when over the cap) the log at `path`. The error is a
    /// diagnostic for the tail; a kernel runs without its log rather than not at all.
    pub fn open(path: &Path) -> Result<(Self, Option<String>), String> {
        let mut note = None;
        if let Some(parent) = path.parent() {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;
                builder.mode(0o700);
            }
            builder
                .create(parent)
                .map_err(|error| format!("cannot open kernel stderr log: {error}"))?;
        }
        let mut size = std::fs::metadata(path).map_or(0, |metadata| metadata.len());
        if size > MAX_LOG_BYTES {
            let old = rotated(path);
            // Tighten before the move: a renamed log keeps its mode.
            restrict(path);
            let _ = std::fs::remove_file(&old);
            match std::fs::rename(path, &old) {
                Ok(()) => size = 0,
                Err(error) => note = Some(format!("cannot rotate kernel stderr log: {error}")),
            }
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options
            .open(path)
            .map_err(|error| format!("cannot open kernel stderr log: {error}"))?;
        // Exact bits despite the umask, and a loose log from before is tightened.
        restrict(path);
        Ok((
            Self {
                file,
                budget: MAX_LOG_BYTES.saturating_sub(size),
                writable: true,
            },
            note,
        ))
    }

    /// Append what the kernel wrote; once the budget is spent a marker ends the file
    /// and the rest is dropped.
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        if !self.writable {
            return Ok(());
        }
        let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let written = if length <= self.budget {
            self.budget -= length;
            self.file.write_all(bytes)
        } else {
            self.writable = false;
            self.file.write_all(BUDGET_MARKER.as_bytes())
        };
        written.map_err(|error| {
            self.writable = false;
            format!("kernel stderr log write failed: {error}")
        })
    }
}

fn rotated(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".old");
    PathBuf::from(name)
}

/// Owner-only bits on POSIX; on Windows the per-user data directory's ACL is
/// already the owner's.
fn restrict(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// Read the kernel's stderr until it closes: every chunk into `push` as text (a
/// character split across chunks is kept for the next one) and into the log.
pub async fn drain(
    mut stderr: impl AsyncRead + Unpin,
    mut log: Option<Log>,
    mut push: impl FnMut(&str),
) {
    let mut buffer = vec![0_u8; 8 * 1024];
    let mut carry: Vec<u8> = Vec::new();
    loop {
        let read = match stderr.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let chunk = &buffer[..read];
        if let Some(file) = log.as_mut()
            && let Err(error) = file.write(chunk)
        {
            push(&format!("[kernel] {error}\n"));
        }
        carry.extend_from_slice(chunk);
        let complete = match std::str::from_utf8(&carry) {
            Ok(text) => text.len(),
            // Only an incomplete final character waits; invalid bytes are shown lossily.
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(_) => carry.len(),
        };
        push(&String::from_utf8_lossy(&carry[..complete]));
        carry.drain(..complete);
    }
    if !carry.is_empty() {
        push(&String::from_utf8_lossy(&carry));
    }
}

#[cfg(test)]
mod tests {
    use super::{LOG_FILE, Log, MAX_LOG_BYTES, MAX_TAIL_CHARS, Tail};

    #[test]
    fn the_tail_keeps_only_the_last_characters() {
        let mut tail = Tail::default();
        tail.push(&"a".repeat(MAX_TAIL_CHARS));
        tail.push("ée🙂end");
        assert_eq!(tail.last(usize::MAX).chars().count(), MAX_TAIL_CHARS);
        assert!(tail.last(usize::MAX).ends_with("ée🙂end"));
        assert_eq!(tail.last(3), "end");
    }

    #[test]
    fn the_log_rotates_when_over_the_cap_and_stops_at_its_budget() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("kernel").join(LOG_FILE);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(
            &path,
            vec![b'x'; usize::try_from(MAX_LOG_BYTES).expect("size") + 1],
        )
        .expect("big log");
        let (mut log, note) = Log::open(&path).expect("log");
        assert!(note.is_none());
        let mut old = path.as_os_str().to_owned();
        old.push(".old");
        assert!(
            std::path::Path::new(&old).is_file(),
            "the big log moved aside"
        );
        log.write(b"first\n").expect("write");
        log.budget = 3;
        log.write(b"too long").expect("marker");
        log.write(b"dropped").expect("dropped");
        assert_eq!(
            std::fs::read_to_string(&path).expect("log"),
            "first\n[stderr log budget exhausted]\n"
        );
    }

    #[tokio::test]
    async fn a_character_split_across_reads_is_kept_whole() {
        let bytes = "héllo".as_bytes().to_vec();
        let (mut writer, reader) = tokio::io::duplex(64);
        let feeder = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt as _;
            writer.write_all(&bytes[..2]).await.expect("write");
            writer.flush().await.expect("flush");
            tokio::task::yield_now().await;
            writer.write_all(&bytes[2..]).await.expect("write");
        });
        let mut text = String::new();
        super::drain(reader, None, |chunk| text.push_str(chunk)).await;
        feeder.await.expect("feeder");
        assert_eq!(text, "héllo");
    }
}
