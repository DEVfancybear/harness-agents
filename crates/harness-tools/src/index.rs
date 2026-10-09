//! The n-gram index that keeps `search_text` from reading every file of a
//! large workspace.
//!
//! Each indexed file keeps a Bloom filter of its trigrams (ASCII-lowercased
//! byte triples that do not cross a line end). A search turns its pattern into
//! the literals any match must contain - `foo.*bar` needs `foo` and `bar`,
//! `(get|set)_name` needs `get_name` or `set_name` - and reads only the files
//! whose filter holds every trigram of those literals. A filter never answers
//! "no" for a trigram the file has, so the index can only spare reads: the
//! regex still decides every match.
//!
//! Filters are sized to their file (about four bits per distinct trigram), so
//! the index stays near half a byte per trigram where posting lists would take
//! eight or more; one file changing rebuilds one filter. The index is used for
//! a workspace of at least [`INDEX_MIN_FILES`] files (`HA_SEARCH_INDEX=always`
//! or `off` overrides that), built in the background the first time such a
//! workspace is searched, and brought up to date from the shared walk: a file
//! whose size or time differs from its filter, or that changed within the last
//! two seconds, is read as if there were no index until it is indexed again.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, RwLock},
    time::{Duration, SystemTime},
};

use regex_syntax::hir::{Class, Hir, HirKind};

use crate::{
    search::Matchers,
    walk::{WalkFile, cached_walk},
    workspace::MAX_TEXT_FILE_BYTES,
};

/// Workspaces with fewer files are searched by reading them: under this size
/// the index costs more to keep than it saves.
pub(crate) const INDEX_MIN_FILES: usize = 5_000;

/// A file modified this recently may change again within one timestamp tick,
/// so its filter is not trusted yet ("racily clean", as `git` calls it).
const RACY_WINDOW: Duration = Duration::from_secs(2);

/// Most literals one clause of a query may expand to before it is dropped.
const MAX_ALTERNATIVES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Off,
    Auto,
    Always,
}

fn mode() -> Mode {
    match std::env::var("HA_SEARCH_INDEX").as_deref() {
        Ok("off" | "0" | "false") => Mode::Off,
        Ok("always" | "on" | "1" | "true") => Mode::Always,
        _ => Mode::Auto,
    }
}

// ---------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------

struct Filter {
    len: u64,
    modified: Option<SystemTime>,
    indexed_at: SystemTime,
    /// `None`: a file the search skips anyway (too large, binary, not UTF-8).
    bits: Option<Box<[u64]>>,
}

impl Filter {
    /// Whether this filter still describes `file`.
    fn current(&self, file: &WalkFile) -> bool {
        self.len == file.len
            && self.modified == file.modified
            && self.modified.is_some_and(|modified| {
                self.indexed_at
                    .duration_since(modified)
                    .is_ok_and(|age| age >= RACY_WINDOW)
            })
    }

    fn may_contain_all(&self, trigrams: &[u32]) -> bool {
        let Some(bits) = &self.bits else {
            // A file the search would skip can hold no match.
            return false;
        };
        trigrams.iter().all(|trigram| {
            let bit = slot(*trigram, bits.len());
            bits[bit / 64] & (1 << (bit % 64)) != 0
        })
    }
}

fn slot(trigram: u32, words: usize) -> usize {
    let hashed = trigram.wrapping_mul(0x9E37_79B1).rotate_left(7) ^ trigram;
    (hashed as usize) % (words * 64)
}

fn trigram(window: &[u8]) -> u32 {
    (u32::from(window[0].to_ascii_lowercase()) << 16)
        | (u32::from(window[1].to_ascii_lowercase()) << 8)
        | u32::from(window[2].to_ascii_lowercase())
}

/// The filter of one file's bytes.
fn filter_of(bytes: &[u8]) -> Box<[u64]> {
    let mut trigrams = bytes
        .windows(3)
        .filter(|window| !window.contains(&b'\n'))
        .map(trigram)
        .collect::<Vec<_>>();
    trigrams.sort_unstable();
    trigrams.dedup();
    let words = (trigrams.len() * 4)
        .div_ceil(64)
        .next_power_of_two()
        .clamp(16, 1024);
    let mut bits = vec![0_u64; words].into_boxed_slice();
    for value in trigrams {
        let bit = slot(value, words);
        bits[bit / 64] |= 1 << (bit % 64);
    }
    bits
}

fn index_file(file: &WalkFile) -> Option<Filter> {
    let indexed_at = SystemTime::now();
    let bits = if file.len <= MAX_TEXT_FILE_BYTES as u64 {
        // A transient read failure is not evidence that a file has no match.
        // Leave it unindexed so searches read it and later refreshes retry it.
        let bytes = std::fs::read(&file.absolute).ok()?;
        (bytes.len() <= MAX_TEXT_FILE_BYTES
            && memchr::memchr(0, &bytes).is_none()
            && std::str::from_utf8(&bytes).is_ok())
        .then(|| filter_of(&bytes))
    } else {
        None
    };
    Some(Filter {
        len: file.len,
        modified: file.modified,
        indexed_at,
        bits,
    })
}

// ---------------------------------------------------------------------------
// The index of one workspace
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Index {
    filters: HashMap<PathBuf, Filter>,
    ready: bool,
    /// A refresh thread is running.
    refreshing: bool,
}

/// The whole workspace, as the shared walk has it, bounded like a tool walk.
fn root_walk(root: &Path) -> Result<Arc<crate::walk::Walk>, harness_types::HarnessError> {
    cached_walk(
        root,
        root,
        Some(std::time::Instant::now() + crate::walk::TOOL_WALK_DEADLINE),
    )
}

fn indexes() -> &'static Mutex<HashMap<PathBuf, Arc<RwLock<Index>>>> {
    static INDEXES: OnceLock<Mutex<HashMap<PathBuf, Arc<RwLock<Index>>>>> = OnceLock::new();
    INDEXES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Index `files` that the index does not describe, on all cores.
fn refresh(index: &RwLock<Index>, files: &[WalkFile]) {
    let stale = {
        let index = index
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        files
            .iter()
            .filter(|file| {
                index
                    .filters
                    .get(&file.absolute)
                    .is_none_or(|filter| !filter.current(file))
            })
            .cloned()
            .collect::<Vec<_>>()
    };
    let threads = std::thread::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .clamp(1, 8);
    let chunk = stale.len().div_ceil(threads).max(1);
    let built = std::thread::scope(|scope| {
        stale
            .chunks(chunk)
            .map(|part| {
                scope.spawn(move || {
                    part.iter()
                        .map(|file| (file.absolute.clone(), index_file(file)))
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|handle| handle.join().unwrap_or_default())
            .collect::<Vec<_>>()
    });
    let mut index = index
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let live = files
        .iter()
        .map(|file| file.absolute.as_path())
        .collect::<std::collections::HashSet<_>>();
    index
        .filters
        .retain(|path, _| live.contains(path.as_path()));
    for (path, filter) in built {
        if let Some(filter) = filter {
            index.filters.insert(path, filter);
        } else {
            index.filters.remove(&path);
        }
    }
    index.ready = true;
    index.refreshing = false;
}

/// Bring the index of `root` up to date in the background.
fn refresh_in_background(root: &Path, index: &Arc<RwLock<Index>>) {
    {
        let mut guard = index
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.refreshing {
            return;
        }
        guard.refreshing = true;
    }
    let root = root.to_owned();
    let owned = Arc::clone(index);
    let spawned = std::thread::Builder::new()
        .name("ha-search-index".to_owned())
        .spawn(move || match root_walk(&root) {
            Ok(walk) if walk.complete => refresh(&owned, &walk.files),
            _ => {
                owned
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .refreshing = false;
            }
        });
    if spawned.is_err() {
        index
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .refreshing = false;
    }
}

/// The positions in `files` worth reading for `matchers`, or `None` to read
/// them all: no index for this workspace (yet), or a pattern that requires
/// no literal of three characters.
pub(crate) fn candidates(
    root: &Path,
    files: &[&WalkFile],
    matchers: &Matchers,
    case_insensitive: bool,
) -> Option<Vec<usize>> {
    let mode = mode();
    if mode == Mode::Off {
        return None;
    }
    let query = Query::from_pattern(&matchers.pattern, case_insensitive)?;
    let index = {
        let mut all = indexes()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(index) = all.get(root) {
            Arc::clone(index)
        } else {
            let large = mode == Mode::Always
                || root_walk(root).is_ok_and(|walk| walk.files.len() >= INDEX_MIN_FILES);
            if !large {
                return None;
            }
            let index = Arc::new(RwLock::new(Index::default()));
            all.insert(root.to_owned(), Arc::clone(&index));
            index
        }
    };
    if mode == Mode::Always && !index.read().is_ok_and(|index| index.ready) {
        // A forced index is built before its first answer, so a test sees it.
        if let Ok(walk) = root_walk(root) {
            refresh(&index, &walk.files);
        }
    }
    let guard = index
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !guard.ready {
        drop(guard);
        refresh_in_background(root, &index);
        return None;
    }
    let mut stale = false;
    let chosen = files
        .iter()
        .enumerate()
        .filter(|(_, file)| match guard.filters.get(&file.absolute) {
            Some(filter) if filter.current(file) => query.admits(filter),
            _ => {
                stale = true;
                true
            }
        })
        .map(|(position, _)| position)
        .collect();
    drop(guard);
    if stale {
        refresh_in_background(root, &index);
    }
    Some(chosen)
}

// ---------------------------------------------------------------------------
// Queries: the literals a pattern requires
// ---------------------------------------------------------------------------

/// What a match must contain: every clause holds, and a clause holds when
/// one of its literals does (each literal as the trigrams it is made of).
#[derive(Debug, PartialEq)]
pub(crate) struct Query {
    clauses: Vec<Vec<Vec<u32>>>,
}

impl Query {
    fn admits(&self, filter: &Filter) -> bool {
        self.clauses.iter().all(|clause| {
            clause
                .iter()
                .any(|trigrams| filter.may_contain_all(trigrams))
        })
    }

    /// `None` when the pattern requires nothing the index can check.
    pub(crate) fn from_pattern(pattern: &str, case_insensitive: bool) -> Option<Self> {
        // Parsed case-sensitively: the index folds ASCII case itself, and a
        // case-insensitive parse would turn every letter into a class.
        let hir = regex_syntax::ParserBuilder::new()
            .build()
            .parse(pattern)
            .ok()?;
        let info = analyze(&hir);
        let mut clauses = info.required;
        if let Some(exact) = info.exact {
            clauses.push(exact);
        }
        let clauses = clauses
            .into_iter()
            .filter_map(|clause| {
                clause
                    .iter()
                    .map(|literal| literal_trigrams(literal, case_insensitive))
                    .collect::<Option<Vec<_>>>()
            })
            .filter(|clause| !clause.is_empty())
            .collect::<Vec<_>>();
        (!clauses.is_empty()).then_some(Self { clauses })
    }
}

/// The trigrams of one required literal, or `None` when the literal is too
/// short to require any (the clause then requires nothing).
///
/// Under case-insensitive matching `k` and `s` also match the Kelvin sign and
/// the long s, which are not ASCII: a trigram holding either is not required.
fn literal_trigrams(literal: &[u8], case_insensitive: bool) -> Option<Vec<u32>> {
    let trigrams = literal
        .windows(3)
        .filter(|window| !window.contains(&b'\n'))
        .filter(|window| {
            !case_insensitive
                || !window.iter().any(|byte| {
                    matches!(byte.to_ascii_lowercase(), b'k' | b's') || !byte.is_ascii()
                })
        })
        .map(trigram)
        .collect::<Vec<_>>();
    (!trigrams.is_empty()).then_some(trigrams)
}

/// What one part of a pattern says about its matches.
#[derive(Debug, Default)]
struct Info {
    /// Every string it can match, when there are few enough to list.
    exact: Option<Vec<Vec<u8>>>,
    /// Clauses every match must satisfy.
    required: Vec<Vec<Vec<u8>>>,
}

fn product(left: &[Vec<u8>], right: &[Vec<u8>]) -> Option<Vec<Vec<u8>>> {
    if left.len().saturating_mul(right.len()) > MAX_ALTERNATIVES {
        return None;
    }
    Some(
        left.iter()
            .flat_map(|head| {
                right.iter().map(move |tail| {
                    let mut joined = head.clone();
                    joined.extend_from_slice(tail);
                    joined
                })
            })
            .collect(),
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "one arm per kind of pattern node, read together"
)]
fn analyze(hir: &Hir) -> Info {
    match hir.kind() {
        HirKind::Empty | HirKind::Look(_) => Info {
            exact: Some(vec![Vec::new()]),
            required: Vec::new(),
        },
        HirKind::Literal(literal) => Info {
            exact: Some(vec![literal.0.to_vec()]),
            required: Vec::new(),
        },
        HirKind::Class(class) => Info {
            exact: class_strings(class),
            required: Vec::new(),
        },
        HirKind::Capture(capture) => analyze(&capture.sub),
        HirKind::Repetition(repetition) => {
            if repetition.min == 0 {
                return Info::default();
            }
            let inner = analyze(&repetition.sub);
            let mut required = inner.required;
            if repetition.min == 1 && repetition.max == Some(1) {
                return Info {
                    exact: inner.exact,
                    required,
                };
            }
            if let Some(exact) = inner.exact {
                required.push(exact);
            }
            Info {
                exact: None,
                required,
            }
        }
        HirKind::Concat(parts) => {
            let mut required = Vec::new();
            let mut run: Option<Vec<Vec<u8>>> = Some(vec![Vec::new()]);
            let mut whole = true;
            for part in parts {
                let info = analyze(part);
                required.extend(info.required);
                match (run.take(), info.exact) {
                    (Some(current), Some(exact)) => {
                        run = product(&current, &exact);
                        if run.is_none() {
                            // Too many combinations: keep what each side requires.
                            whole = false;
                            required.push(current);
                            run = Some(exact);
                        }
                    }
                    (Some(current), None) => {
                        whole = false;
                        required.push(current);
                        run = Some(vec![Vec::new()]);
                    }
                    (None, exact) => {
                        whole = false;
                        run = exact.or_else(|| Some(vec![Vec::new()]));
                    }
                }
            }
            if whole {
                return Info {
                    exact: run,
                    required,
                };
            }
            if let Some(current) = run {
                required.push(current);
            }
            Info {
                exact: None,
                required,
            }
        }
        HirKind::Alternation(branches) => {
            let infos = branches.iter().map(analyze).collect::<Vec<_>>();
            if infos.iter().all(|info| info.exact.is_some()) {
                let exact = infos
                    .into_iter()
                    .flat_map(|info| info.exact.unwrap_or_default())
                    .collect::<Vec<_>>();
                return Info {
                    exact: (exact.len() <= MAX_ALTERNATIVES).then_some(exact),
                    required: Vec::new(),
                };
            }
            // Each branch must bring one literal of its own: the clause is the
            // union of one literal set per branch.
            let mut clause = Vec::new();
            for info in infos {
                let chosen = info
                    .required
                    .into_iter()
                    .chain(info.exact)
                    .filter(|set| set.iter().all(|literal| literal.len() >= 3))
                    .min_by_key(Vec::len);
                match chosen {
                    Some(set) => clause.extend(set),
                    None => return Info::default(),
                }
            }
            Info {
                exact: None,
                required: if clause.len() <= MAX_ALTERNATIVES {
                    vec![clause]
                } else {
                    Vec::new()
                },
            }
        }
    }
}

/// The strings of a small class (`[ab]`, `[0-3]`), each as UTF-8 bytes.
fn class_strings(class: &Class) -> Option<Vec<Vec<u8>>> {
    match class {
        Class::Unicode(class) => {
            let mut strings = Vec::new();
            for range in class.iter() {
                let count = u32::from(range.end()) - u32::from(range.start()) + 1;
                if strings.len() + count as usize > 8 {
                    return None;
                }
                for value in u32::from(range.start())..=u32::from(range.end()) {
                    let character = char::from_u32(value)?;
                    strings.push(character.to_string().into_bytes());
                }
            }
            Some(strings)
        }
        Class::Bytes(class) => {
            let mut strings = Vec::new();
            for range in class.iter() {
                let count = usize::from(range.end() - range.start()) + 1;
                if strings.len() + count > 8 {
                    return None;
                }
                for value in range.start()..=range.end() {
                    strings.push(vec![value]);
                }
            }
            Some(strings)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn literals(pattern: &str) -> Option<Vec<Vec<String>>> {
        let hir = regex_syntax::ParserBuilder::new()
            .build()
            .parse(pattern)
            .ok()?;
        let info = analyze(&hir);
        let mut clauses = info.required;
        clauses.extend(info.exact);
        let mut clauses = clauses
            .into_iter()
            .map(|clause| {
                let mut words = clause
                    .into_iter()
                    .map(|literal| String::from_utf8(literal).unwrap_or_default())
                    .collect::<Vec<_>>();
                words.sort();
                words
            })
            .filter(|clause| !clause.iter().any(String::is_empty))
            .collect::<Vec<_>>();
        clauses.sort();
        Some(clauses)
    }

    #[test]
    fn a_pattern_requires_the_literals_every_match_holds() {
        assert_eq!(
            literals("hello world"),
            Some(vec![vec!["hello world".to_owned()]])
        );
        assert_eq!(
            literals("foo.*bar"),
            Some(vec![vec!["bar".to_owned()], vec!["foo".to_owned()]])
        );
        assert_eq!(
            literals("(get|set)_name"),
            Some(vec![vec!["get_name".to_owned(), "set_name".to_owned()]])
        );
        assert_eq!(
            literals(r"fn compute_\d+_11\("),
            Some(vec![
                vec!["_11(".to_owned()],
                vec!["fn compute_".to_owned()]
            ])
        );
        assert_eq!(
            literals("a+b"),
            Some(vec![vec!["a".to_owned()], vec!["b".to_owned()]])
        );
    }

    #[test]
    fn a_filter_never_denies_a_trigram_its_file_holds() {
        let text = b"pub fn Compute_Total(input: &[u64]) -> u64 { input.iter().sum() }\n";
        let filter = Filter {
            len: 0,
            modified: None,
            indexed_at: SystemTime::now(),
            bits: Some(filter_of(text)),
        };
        for pattern in [
            "compute_total",
            "COMPUTE",
            "iter().sum",
            r"fn \w+\(",
            "input|nothing",
        ] {
            if let Some(query) = Query::from_pattern(pattern, true) {
                assert!(query.admits(&filter), "{pattern} must be admitted");
            }
        }
        let absent = Query::from_pattern("zebra_crossing", false).expect("literal");
        assert!(
            !absent.admits(&filter),
            "a literal the file lacks is (almost surely) denied"
        );
        let skip = Filter {
            bits: None,
            ..filter
        };
        assert!(!absent.admits(&skip));
    }

    #[test]
    fn a_failed_read_is_retried_when_the_file_returns_unchanged() {
        let temp = tempfile::tempdir().expect("temp root");
        let path = temp.path().join("source.txt");
        std::fs::write(&path, "needle").expect("source");
        let modified = SystemTime::now() - Duration::from_secs(10);
        let set_time = || {
            std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .expect("open source")
                .set_times(std::fs::FileTimes::new().set_modified(modified))
                .expect("source time");
        };
        set_time();
        let metadata = std::fs::metadata(&path).expect("metadata");
        let file = WalkFile {
            absolute: path.clone(),
            relative: "source.txt".to_owned(),
            len: metadata.len(),
            modified: metadata.modified().ok(),
        };
        // The file disappears after the walk but before the index reads it.
        std::fs::remove_file(&path).expect("remove source");
        let index = RwLock::new(Index::default());
        refresh(&index, std::slice::from_ref(&file));
        std::fs::write(&path, "needle").expect("restore source");
        set_time();
        let guard = index.read().expect("index");
        assert!(
            guard
                .filters
                .get(&path)
                .is_none_or(|filter| !filter.current(&file)),
            "a failed read must not become a current negative filter"
        );
        drop(guard);
        refresh(&index, std::slice::from_ref(&file));
        let query = Query::from_pattern("needle", false).expect("query");
        let guard = index.read().expect("refreshed index");
        let filter = guard.filters.get(&path).expect("restored file indexed");
        assert!(filter.current(&file));
        assert!(
            query.admits(filter),
            "the restored file must remain searchable"
        );
    }

    #[test]
    fn short_or_open_patterns_use_no_index() {
        for pattern in ["ab", r"\w+", ".*", "a|bc", "[a-z]+"] {
            assert_eq!(Query::from_pattern(pattern, false), None, "{pattern}");
        }
    }
}
