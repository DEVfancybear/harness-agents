//! Discover only running, matching loopback consoles for native page citations.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use harness_types::{ContentHash, TaskId};
use serde::{Deserialize, Serialize};

const DIRECTORY: &str = ".ha-consoles";
const MAX_ENTRIES: usize = 64;
const MAX_ENTRY_BYTES: u64 = 2048;
const MAX_RESPONSE_BYTES: u64 = 4096;
const PROBE_TIMEOUT: Duration = Duration::from_millis(150);
const LOOKUP_BUDGET: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Identity {
    schema_version: u32,
    nonce: String,
    project: String,
    global: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    identity: Identity,
    address: SocketAddr,
    created_ms: u128,
}

/// Only the server that created a nonce may remove its registration on exit.
pub(crate) struct Registration {
    path: PathBuf,
    lease: Option<File>,
    pub(crate) identity: Identity,
}

impl Drop for Registration {
    fn drop(&mut self) {
        if read_entry(&self.path).is_some_and(|entry| entry.identity.nonce == self.identity.nonce) {
            let _ = fs::remove_file(&self.path);
        }
        drop(self.lease.take());
        let _ = fs::remove_file(self.path.with_extension("lease"));
    }
}

/// A single bounded lookup supplies URLs for every result in a tool response.
#[derive(Default)]
pub(crate) struct ConsoleLinks {
    project: Option<SocketAddr>,
    global: Option<SocketAddr>,
}

impl ConsoleLinks {
    pub(crate) fn discover(project_root: &Path, global_root: &Path) -> Self {
        let Some((project, global, directory)) = roots(project_root, global_root) else {
            return Self::default();
        };
        let Some(mut entries) = entries(&directory) else {
            return Self::default();
        };
        entries.retain(|(_, entry)| entry.identity.global == global);
        // Prefer the newest matching project; a global page can also be opened
        // by another project console backed by the exact same global library.
        entries.sort_by(|(_, left), (_, right)| {
            (
                right.identity.project == project,
                right.created_ms,
                &right.identity.nonce,
            )
                .cmp(&(
                    left.identity.project == project,
                    left.created_ms,
                    &left.identity.nonce,
                ))
        });
        let deadline = Instant::now() + LOOKUP_BUDGET;
        let mut links = Self::default();
        for (path, entry) in entries {
            if Instant::now() >= deadline || (links.project.is_some() && links.global.is_some()) {
                break;
            }
            let matches_project = entry.identity.project == project;
            if (!matches_project || links.project.is_some()) && links.global.is_some() {
                continue;
            }
            if lease_active(&path) == Some(true) && probe(&entry, deadline) {
                if matches_project && links.project.is_none() {
                    links.project = Some(entry.address);
                }
                if links.global.is_none() {
                    links.global = Some(entry.address);
                }
            }
        }
        links
    }

    pub(crate) fn page_url(&self, scope: &str, path: &str) -> Option<String> {
        // Native callers normally supply validated store paths. Keep this API
        // independently safe against invalid or ambiguous deep-link parameters.
        if path.is_empty()
            || path.len() > 512
            || path.starts_with(['/', '\\'])
            || path.contains('\\')
            || path.chars().any(char::is_control)
            || path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return None;
        }
        let address = match scope {
            "project" => self.project?,
            "global" => self.global?,
            _ => return None,
        };
        Some(format!(
            "http://{address}/?scope={scope}&page={}",
            encode(path)
        ))
    }
}

pub(crate) fn register(
    project_root: &Path,
    global_root: &Path,
    address: SocketAddr,
) -> Result<Registration, String> {
    if !address.ip().is_loopback() || address.port() == 0 {
        return Err("console registry requires a bound loopback address".to_owned());
    }
    let (project, global, directory) = roots(project_root, global_root)
        .ok_or("console registry paths are unavailable or unsafe")?;
    validate_path(&directory)?;
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    validate_path(&directory)?;
    let mut registered = entries(&directory).ok_or("console registry exceeds its entry limit")?;
    // An OS-held lease is released even when a foreground process is killed.
    // Reclaim only released leases; a busy server keeps its registration.
    if registered.len() >= MAX_ENTRIES {
        for (path, entry) in &registered {
            if lease_active(path) == Some(false)
                && read_entry(path)
                    .is_some_and(|current| current.identity.nonce == entry.identity.nonce)
                && fs::remove_file(path).is_ok()
            {
                let _ = fs::remove_file(path.with_extension("lease"));
                break; // One free slot is sufficient for this registration.
            }
        }
        registered = entries(&directory).ok_or("console registry exceeds its entry limit")?;
    }
    if registered.len() >= MAX_ENTRIES {
        return Err("console registry is full".to_owned());
    }
    let nonce = hash_hex(format!("{}:{}", TaskId::generate(), TaskId::generate()).as_bytes());
    let identity = Identity {
        schema_version: 1,
        nonce: nonce.clone(),
        project,
        global,
    };
    let entry = Entry {
        identity: identity.clone(),
        address,
        created_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    };
    let path = directory.join(format!("{nonce}.json"));
    let lease_path = path.with_extension("lease");
    let lease = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&lease_path)
        .map_err(|error| error.to_string())?;
    lease.try_lock().map_err(|error| error.to_string())?;
    let temporary = directory.join(format!(".{nonce}.tmp"));
    let result = (|| {
        let bytes = serde_json::to_vec(&entry).map_err(|error| error.to_string())?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        file.write_all(&bytes).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        drop(file);
        fs::rename(&temporary, &path).map_err(|error| error.to_string())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        drop(lease);
        let _ = fs::remove_file(&lease_path);
        return Err(error);
    }
    Ok(Registration {
        path,
        lease: Some(lease),
        identity,
    })
}

impl Identity {
    pub(crate) fn accepts_probe(&self, nonce: Option<&String>) -> bool {
        nonce == Some(&self.nonce)
    }
}

fn roots(project_root: &Path, global_root: &Path) -> Option<(String, String, PathBuf)> {
    let project = root_identity(project_root)?;
    let global = root_identity(global_root)?;
    let directory = global_root.join(DIRECTORY);
    validate_path(&directory).ok()?;
    Some((project, global, directory))
}

fn root_identity(path: &Path) -> Option<String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    if absolute
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return None;
    }
    validate_path(&absolute).ok()?;
    let mut nearest = absolute.as_path();
    let mut tail = Vec::new();
    while !nearest.exists() {
        tail.push(nearest.file_name()?.to_owned());
        nearest = nearest.parent()?;
    }
    let mut resolved = nearest.canonicalize().ok()?;
    for part in tail.into_iter().rev() {
        resolved.push(part);
    }
    let text = resolved.to_str()?;
    #[cfg(windows)]
    let text = text.to_lowercase();
    Some(hash_hex(text.as_bytes()))
}

fn hash_hex(bytes: &[u8]) -> String {
    ContentHash::from_bytes(bytes)
        .as_str()
        .trim_start_matches("sha256:")
        .to_owned()
}

fn validate_path(path: &Path) -> Result<(), String> {
    for (depth, ancestor) in path.ancestors().enumerate() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                #[cfg(windows)]
                let reparse = {
                    use std::os::windows::fs::MetadataExt;
                    metadata.file_attributes() & 0x400 != 0
                };
                #[cfg(not(windows))]
                let reparse = false;
                if metadata.file_type().is_symlink() || reparse {
                    return Err(
                        "console registry cannot contain symlinks or reparse points".to_owned()
                    );
                }
                if depth > 0 && !metadata.is_dir() {
                    return Err("console registry parent is not a directory".to_owned());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

fn entries(directory: &Path) -> Option<Vec<(PathBuf, Entry)>> {
    validate_path(directory).ok()?;
    let directory = match fs::read_dir(directory) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(Vec::new()),
        Err(_) => return None,
    };
    let mut result = Vec::new();
    for (count, file) in directory.enumerate() {
        if count >= MAX_ENTRIES * 2 {
            return None;
        }
        let path = file.ok()?.path();
        if let Some(entry) = read_entry(&path) {
            result.push((path, entry));
        }
    }
    Some(result)
}

fn lease_active(path: &Path) -> Option<bool> {
    let path = path.with_extension("lease");
    validate_path(&path).ok()?;
    if !fs::symlink_metadata(&path).ok()?.is_file() {
        return None;
    }
    let file = OpenOptions::new().read(true).write(true).open(path).ok()?;
    match file.try_lock() {
        Ok(()) => Some(false),
        Err(fs::TryLockError::WouldBlock) => Some(true),
        Err(fs::TryLockError::Error(_)) => None,
    }
}

fn read_entry(path: &Path) -> Option<Entry> {
    validate_path(path).ok()?;
    let filename = path.file_name()?.to_str()?;
    let nonce = filename.strip_suffix(".json")?;
    if !digest(nonce) {
        return None;
    }
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_ENTRY_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(MAX_ENTRY_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_ENTRY_BYTES {
        return None;
    }
    let entry: Entry = serde_json::from_slice(&bytes).ok()?;
    (entry.identity.schema_version == 1
        && entry.identity.nonce == nonce
        && digest(&entry.identity.project)
        && digest(&entry.identity.global)
        && entry.address.ip().is_loopback()
        && entry.address.port() > 0)
        .then_some(entry)
}

fn digest(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn probe(entry: &Entry, deadline: Instant) -> bool {
    let deadline = deadline.min(Instant::now() + PROBE_TIMEOUT);
    let timeout = deadline.saturating_duration_since(Instant::now());
    if timeout.is_zero() {
        return false;
    }
    let Ok(mut socket) = TcpStream::connect_timeout(&entry.address, timeout) else {
        return false;
    };
    if socket.set_write_timeout(Some(timeout)).is_err() {
        return false;
    }
    let request = format!(
        "GET /api/identity HTTP/1.1\r\nHost: {}\r\nX-HA-Console-Probe: {}\r\nConnection: close\r\n\r\n",
        entry.address, entry.identity.nonce
    );
    if socket.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut response = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || socket.set_read_timeout(Some(remaining)).is_err() {
            return false;
        }
        let mut buffer = [0; 512];
        match socket.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => response.extend_from_slice(&buffer[..count]),
            Err(_) => return false,
        }
        if response.len() as u64 > MAX_RESPONSE_BYTES {
            return false;
        }
    }
    let Some(head_end) = response.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
        return false;
    };
    let Ok(head) = std::str::from_utf8(&response[..head_end]) else {
        return false;
    };
    if !head.starts_with("HTTP/1.1 200 ") {
        return false;
    }
    let Ok(identity) = serde_json::from_slice::<Identity>(&response[head_end + 4..]) else {
        return false;
    };
    identity.schema_version == 1
        && identity.nonce == entry.identity.nonce
        && identity.project == entry.identity.project
        && identity.global == entry.identity.global
}

fn encode(text: &str) -> String {
    use std::fmt::Write as _;
    let mut result = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            result.push(char::from(byte));
        } else {
            let _ = write!(result, "%{byte:02X}");
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv6Addr, TcpListener};

    fn fixture_listener() -> TcpListener {
        TcpListener::bind((Ipv6Addr::LOCALHOST, 0))
            .or_else(|_| TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)))
            .unwrap()
    }

    fn serve_identity(listener: TcpListener, identity: Identity) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 512];
            while !bytes.windows(4).any(|part| part == b"\r\n\r\n") {
                let count = socket.read(&mut buffer).unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
                assert!(bytes.len() <= 2048);
            }
            let request = std::str::from_utf8(&bytes).unwrap();
            assert!(request.contains(&identity.nonce));
            let body = serde_json::to_string(&identity).unwrap();
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            socket.shutdown(std::net::Shutdown::Write).unwrap();
        })
    }

    #[test]
    fn unicode_and_reserved_page_characters_are_encoded_once() {
        let links = ConsoleLinks {
            project: Some("[::1]:1234".parse().unwrap()),
            global: None,
        };
        assert_eq!(
            links.page_url("project", "sources/知识 a&b?#%.md").unwrap(),
            "http://[::1]:1234/?scope=project&page=sources%2F%E7%9F%A5%E8%AF%86%20a%26b%3F%23%25.md"
        );
        for path in ["", "../a.md", "/a.md", "a\\b.md", "a//b.md", "a\nb.md"] {
            assert!(links.page_url("project", path).is_none());
        }
        assert!(links.page_url("global", "a.md").is_none());
        assert!(links.page_url("both", "a.md").is_none());
    }

    #[test]
    fn matching_live_console_is_discovered_and_own_registration_is_cleaned() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let global = temp.path().join("global");
        let listener = fixture_listener();
        let address = listener.local_addr().unwrap();
        let registration = register(&project, &global, address).unwrap();
        let path = registration.path.clone();
        let server = serve_identity(listener, registration.identity.clone());
        let links = ConsoleLinks::discover(&project, &global);
        assert_eq!(links.project, Some(address));
        assert_eq!(links.global, Some(address));
        server.join().unwrap();
        drop(registration);
        assert!(!path.exists());
    }

    #[test]
    fn foreign_project_console_may_only_link_the_matching_global_library() {
        let temp = tempfile::tempdir().unwrap();
        let global = temp.path().join("global");
        let listener = fixture_listener();
        let address = listener.local_addr().unwrap();
        let registration = register(&temp.path().join("project-a"), &global, address).unwrap();
        let server = serve_identity(listener, registration.identity.clone());
        let links = ConsoleLinks::discover(&temp.path().join("project-b"), &global);
        assert!(links.project.is_none());
        assert_eq!(links.global, Some(address));
        server.join().unwrap();
    }

    #[test]
    fn stale_or_reused_port_cannot_produce_links() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let global = temp.path().join("global");
        let listener = fixture_listener();
        let address = listener.local_addr().unwrap();
        let registration = register(&project, &global, address).unwrap();
        let mut wrong_identity = registration.identity.clone();
        wrong_identity.project = "0".repeat(64);
        let server = serve_identity(listener, wrong_identity);
        let links = ConsoleLinks::discover(&project, &global);
        assert!(links.project.is_none());
        assert!(links.global.is_none());
        server.join().unwrap();
        let links = ConsoleLinks::discover(&project, &global);
        assert!(links.project.is_none());
    }

    #[test]
    fn newest_matching_console_wins_without_removing_other_registrations() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let global = temp.path().join("global");
        let first_listener = fixture_listener();
        let first = register(&project, &global, first_listener.local_addr().unwrap()).unwrap();
        let second_listener = fixture_listener();
        let address = second_listener.local_addr().unwrap();
        let second = register(&project, &global, address).unwrap();
        // Millisecond timestamps can tie, so explicitly make this test's newest
        // entry newer rather than depending on filesystem enumeration order.
        let mut entry = read_entry(&second.path).unwrap();
        entry.created_ms += 1;
        fs::write(&second.path, serde_json::to_vec(&entry).unwrap()).unwrap();
        let server = serve_identity(second_listener, second.identity.clone());
        let links = ConsoleLinks::discover(&project, &global);
        assert_eq!(links.project, Some(address));
        server.join().unwrap();
        drop(second);
        assert!(first.path.exists());
        let server = serve_identity(first_listener, first.identity.clone());
        assert!(ConsoleLinks::discover(&project, &global).project.is_some());
        server.join().unwrap();
    }

    #[test]
    fn malformed_external_or_oversized_registry_is_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let global = temp.path().join("global");
        assert!(register(&project, &global, "192.0.2.1:80".parse().unwrap()).is_err());
        let listener = fixture_listener();
        let registration = register(&project, &global, listener.local_addr().unwrap()).unwrap();
        fs::write(
            &registration.path,
            "x".repeat(usize::try_from(MAX_ENTRY_BYTES).unwrap() + 1),
        )
        .unwrap();
        assert!(ConsoleLinks::discover(&project, &global).project.is_none());
        let directory = global.join(DIRECTORY);
        for count in 0..MAX_ENTRIES * 2 {
            fs::write(directory.join(format!("junk-{count}")), "").unwrap();
        }
        assert!(entries(&directory).is_none());
    }

    #[test]
    fn unresponsive_server_has_an_absolute_probe_deadline() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let global = temp.path().join("global");
        let listener = fixture_listener();
        let registration = register(&project, &global, listener.local_addr().unwrap()).unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            for _ in 0..20 {
                if socket.write_all(b"x").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let started = Instant::now();
        assert!(ConsoleLinks::discover(&project, &global).project.is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
        server.join().unwrap();
        drop(registration);
    }

    #[test]
    fn identity_probe_requires_the_registered_nonce() {
        let identity = Identity {
            schema_version: 1,
            nonce: "1".repeat(64),
            project: "2".repeat(64),
            global: "3".repeat(64),
        };
        assert!(!identity.accepts_probe(None));
        assert!(!identity.accepts_probe(Some(&"4".repeat(64))));
        assert!(identity.accepts_probe(Some(&identity.nonce)));
    }

    #[test]
    fn full_registry_reclaims_abandoned_leases_and_keeps_live_peers() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let global = temp.path().join("global");
        let live_listener = fixture_listener();
        let live = register(&project, &global, live_listener.local_addr().unwrap()).unwrap();
        let stale_listener = fixture_listener();
        let stale_address = stale_listener.local_addr().unwrap();
        let mut stale = Vec::new();
        for _ in 1..MAX_ENTRIES {
            stale.push(register(&project, &global, stale_address).unwrap());
        }
        let new_listener = fixture_listener();
        drop(stale_listener);
        // Simulate the OS releasing file locks after an abrupt process exit,
        // leaving its registry files behind without running the drop guard.
        for registration in &stale {
            registration.lease.as_ref().unwrap().unlock().unwrap();
        }
        let new = register(&project, &global, new_listener.local_addr().unwrap()).unwrap();
        assert!(live.path.exists());
        assert!(new.path.exists());
        assert!(stale.iter().any(|registration| !registration.path.exists()));
        drop(stale);
    }

    #[cfg(unix)]
    #[test]
    fn registry_symlink_and_linked_ancestors_are_refused() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let global = temp.path().join("global");
        fs::create_dir(&global).unwrap();
        symlink(&outside, global.join(DIRECTORY)).unwrap();
        assert!(
            register(
                &temp.path().join("project"),
                &global,
                "127.0.0.1:1234".parse().unwrap()
            )
            .is_err()
        );
        assert!(
            ConsoleLinks::discover(&temp.path().join("project"), &global)
                .project
                .is_none()
        );
    }
}
