//! Versioned filesystem skills and trusted profile composition.
//!
//! A skill is data: it declares an identity, a version, a digest and its
//! provenance, and it may *request* tools and secrets. It can never grant
//! itself authority. Discovery is lazy, content is bounded, and an update only
//! becomes visible at a recorded boundary.

use std::path::{Path, PathBuf};

use harness_session::{
    ContextBlock, ContextBlockKind, ContextContributor, ContextError, ContributorScope,
};
use harness_types::{ContentHash, ErrorCode, SkillId};
use serde::{Deserialize, Serialize};

use crate::contracts::{ExtensionError, MAX_SKILL_BYTES};

/// Bytes of a skill document read to parse its front matter.
///
/// A catalogue scan reads metadata, not content: it needs the declared version
/// and the declared requests, which live in the head of the file. Everything
/// past this bound is never touched during a scan.
pub const MAX_SKILL_HEAD_BYTES: usize = 4 * 1024;

/// Skill documents one catalogue may carry. A directory with more is refused
/// rather than scanned without bound.
pub const MAX_SKILL_CATALOG_ENTRIES: usize = 512;

/// Where a skill came from. Precedence follows the documented merge order:
/// built-in defaults, user config, explicitly trusted project config, selected
/// profile, then CLI overrides.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSource {
    Builtin,
    User,
    TrustedProject,
    Profile,
    CliOverride,
}

impl SkillSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::User => "user",
            Self::TrustedProject => "trusted_project",
            Self::Profile => "profile",
            Self::CliOverride => "cli_override",
        }
    }

    /// Higher precedence wins when two sources define the same skill id.
    #[must_use]
    pub const fn precedence(self) -> u8 {
        match self {
            Self::Builtin => 0,
            Self::User => 1,
            Self::TrustedProject => 2,
            Self::Profile => 3,
            Self::CliOverride => 4,
        }
    }
}

/// A discovered skill and everything needed to reason about its authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillDescriptor {
    pub skill_id: SkillId,
    pub name: String,
    pub version: String,
    pub digest: ContentHash,
    pub source: SkillSource,
    pub path: PathBuf,
    /// Content is data. It is never interpreted as a permission.
    pub content: String,
    /// Tools the skill asks for. A request, not authority.
    pub requested_tools: Vec<String>,
    /// Secret references the skill asks the host to resolve. Never values.
    pub requested_secrets: Vec<String>,
}

impl SkillDescriptor {
    /// Read one skill document from disk with the size bound applied.
    pub fn read(
        path: &Path,
        source: SkillSource,
        requested_tools: Vec<String>,
        requested_secrets: Vec<String>,
    ) -> Result<Self, ExtensionError> {
        let metadata = std::fs::metadata(path).map_err(|error| {
            ExtensionError::new(
                ErrorCode::SkillUnavailable,
                format!("skill {} is unavailable: {error}", path.display()),
            )
        })?;
        if metadata.len() > MAX_SKILL_BYTES as u64 {
            return Err(ExtensionError::new(
                ErrorCode::FrameLimitExceeded,
                format!(
                    "skill {} is {} bytes, over the {MAX_SKILL_BYTES} byte limit",
                    path.display(),
                    metadata.len()
                ),
            ));
        }
        let bytes = std::fs::read(path).map_err(|error| {
            ExtensionError::new(
                ErrorCode::SkillUnavailable,
                format!("skill {} is unavailable: {error}", path.display()),
            )
        })?;
        let content = String::from_utf8(bytes.clone()).map_err(|_| {
            ExtensionError::new(
                ErrorCode::UnsupportedTextEncoding,
                format!("skill {} is not valid UTF-8", path.display()),
            )
        })?;
        let name = front_matter(&content, "name").unwrap_or_else(|| skill_name_from_path(path));
        let version = front_matter(&content, "version").unwrap_or_else(|| "0".to_owned());
        Ok(Self {
            skill_id: SkillId::generate(),
            name,
            version,
            digest: ContentHash::from_bytes(&bytes),
            source,
            path: path.to_path_buf(),
            content,
            requested_tools,
            requested_secrets,
        })
    }

    /// A skill needs an explicit trust grant for every tool and secret it asks
    /// for. Without one the request is refused, and the skill text is still only
    /// data.
    pub fn authorize(
        &self,
        allowed_tools: &[String],
        allowed_secrets: &[String],
    ) -> Result<Vec<String>, ExtensionError> {
        let mut granted = Vec::new();
        for tool in &self.requested_tools {
            if !allowed_tools.iter().any(|allowed| allowed == tool) {
                return Err(ExtensionError::new(
                    ErrorCode::ExtensionCapabilityMismatch,
                    format!(
                        "skill {} requests tool {tool}, which the host grant does not allow",
                        self.name
                    ),
                ));
            }
            granted.push(tool.clone());
        }
        for reference in &self.requested_secrets {
            if !allowed_secrets.iter().any(|allowed| allowed == reference) {
                return Err(ExtensionError::new(
                    ErrorCode::SecretNotGranted,
                    format!(
                        "skill {} requests secret {reference}, which the host grant does not allow",
                        self.name
                    ),
                ));
            }
        }
        Ok(granted)
    }
}

/// A pinned expectation for one skill. A drift is reported, not silently used.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillVersionPin {
    pub name: String,
    pub version: String,
    pub digest: ContentHash,
}

impl SkillVersionPin {
    /// Compare a discovered skill with its pin.
    pub fn check(&self, skill: &SkillDescriptor) -> Result<(), ExtensionError> {
        if skill.name != self.name {
            return Err(ExtensionError::new(
                ErrorCode::SchemaVersionMismatch,
                "skill pin does not describe this skill",
            ));
        }
        if skill.version != self.version || skill.digest != self.digest {
            return Err(ExtensionError::new(
                ErrorCode::SchemaVersionMismatch,
                format!(
                    "skill {} changed from {} ({}) to {} ({})",
                    self.name,
                    self.version,
                    self.digest.as_str(),
                    skill.version,
                    skill.digest.as_str()
                ),
            ));
        }
        Ok(())
    }
}

/// What changed between a pinned skill and a discovered one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillUpdate {
    pub name: String,
    pub from_version: String,
    pub to_version: String,
    pub from_digest: ContentHash,
    pub to_digest: ContentHash,
    /// The boundary at which the update becomes visible.
    pub visible_at_step: u64,
}

impl SkillUpdate {
    pub fn detect(
        pin: &SkillVersionPin,
        skill: &SkillDescriptor,
        visible_at_step: u64,
    ) -> Result<Option<Self>, ExtensionError> {
        if skill.name != pin.name {
            return Err(ExtensionError::new(
                ErrorCode::SchemaVersionMismatch,
                "skill pin does not describe this skill",
            ));
        }
        if skill.version == pin.version && skill.digest == pin.digest {
            return Ok(None);
        }
        Ok(Some(Self {
            name: skill.name.clone(),
            from_version: pin.version.clone(),
            to_version: skill.version.clone(),
            from_digest: pin.digest.clone(),
            to_digest: skill.digest.clone(),
            visible_at_step,
        }))
    }
}

/// Discover skills in one directory, lazily and in a deterministic order.
///
/// Only files carrying a supported extension are considered, and a missing
/// directory is reported as unavailable rather than silently empty.
pub fn discover_skills(
    root: &Path,
    source: SkillSource,
) -> Result<Vec<SkillDescriptor>, ExtensionError> {
    if !root.is_dir() {
        return Err(ExtensionError::new(
            ErrorCode::SkillUnavailable,
            format!("skill source {} does not exist", root.display()),
        ));
    }
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(root).map_err(|error| {
        ExtensionError::new(
            ErrorCode::SkillUnavailable,
            format!("cannot read {}: {error}", root.display()),
        )
    })? {
        let entry = entry.map_err(|error| {
            ExtensionError::new(
                ErrorCode::SkillUnavailable,
                format!("cannot read a skill entry: {error}"),
            )
        })?;
        let path = entry.path();
        if path.is_file()
            && path
                .extension()
                .is_some_and(|ext| ext == "md" || ext == "skill")
        {
            paths.push(path);
        } else if path.is_dir() {
            let document = path.join("SKILL.md");
            if document.is_file() {
                paths.push(document);
            }
        }
    }
    paths.sort();
    paths
        .iter()
        .map(|path| SkillDescriptor::read(path, source, Vec::new(), Vec::new()))
        .collect()
}

/// Compose skills by precedence. A higher-precedence source replaces a
/// lower-precedence skill with the same name; the replaced entries are returned
/// so the decision is inspectable.
pub fn compose_skills(
    candidates: &[SkillDescriptor],
) -> (
    Vec<SkillDescriptor>,
    Vec<(String, SkillSource, SkillSource)>,
) {
    let mut chosen: Vec<SkillDescriptor> = Vec::new();
    let mut replaced = Vec::new();
    let mut ordered = candidates.to_vec();
    ordered.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.source.precedence().cmp(&right.source.precedence()))
    });
    for skill in ordered {
        match chosen
            .iter_mut()
            .find(|existing| existing.name == skill.name)
        {
            Some(existing) => {
                if skill.source.precedence() > existing.source.precedence() {
                    replaced.push((skill.name.clone(), existing.source, skill.source));
                    *existing = skill;
                } else {
                    replaced.push((skill.name.clone(), skill.source, existing.source));
                }
            }
            None => chosen.push(skill),
        }
    }
    chosen.sort_by(|left, right| left.name.cmp(&right.name));
    (chosen, replaced)
}

/// Read a `key: value` front-matter field from a skill document.
fn front_matter(content: &str, key: &str) -> Option<String> {
    let mut lines = content.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':')
            && name.trim() == key
        {
            return Some(value.trim().trim_matches('"').to_owned());
        }
    }
    None
}

fn skill_name_from_path(path: &Path) -> String {
    if path.file_name().is_some_and(|name| name == "SKILL.md")
        && let Some(directory) = path.parent().and_then(Path::file_name)
    {
        return directory.to_string_lossy().into_owned();
    }
    path.file_stem().map_or_else(
        || "unnamed".to_owned(),
        |stem| stem.to_string_lossy().into_owned(),
    )
}

/// Read a comma-separated front-matter list. Absent is empty; a skill that asks
/// for nothing is the common case.
fn front_matter_list(content: &str, key: &str) -> Vec<String> {
    front_matter(content, key).map_or_else(Vec::new, |value| {
        value
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .split(',')
            .map(|item| item.trim().trim_matches('"').trim().to_owned())
            .filter(|item| !item.is_empty())
            .collect()
    })
}

/// One directory the user has explicitly trusted as a skill source.
///
/// Trust is per root, not per file: a repository that adds a skill file cannot
/// widen what was trusted, because a root that was not named is never scanned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustedSkillRoot {
    pub path: PathBuf,
    pub source: SkillSource,
}

impl TrustedSkillRoot {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, source: SkillSource) -> Self {
        Self {
            path: path.into(),
            source,
        }
    }
}

/// A discovered skill as a catalogue carries it: **metadata only**.
///
/// There is deliberately no `content` field. A catalogue scan must be inert —
/// listing skills cannot execute a script, cannot interpret instructions and
/// cannot hold a document in memory — so the body is read only by
/// [`SkillCatalog::activate`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillCatalogEntry {
    pub skill_id: SkillId,
    pub name: String,
    pub description: String,
    pub version: String,
    /// Digest of the whole document at scan time. Streaming, so a scan never
    /// holds the body.
    pub digest: ContentHash,
    pub source: SkillSource,
    pub path: PathBuf,
    pub byte_len: u64,
    /// Tools the skill asks for. A request, never authority.
    pub requested_tools: Vec<String>,
    /// Secret references the skill asks the host to resolve. Never values.
    pub requested_secrets: Vec<String>,
}

impl SkillCatalogEntry {
    /// A stable identity for this exact version of this skill.
    #[must_use]
    pub fn version_ref(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }

    /// Compare a live file against this entry. Any change is reported, never
    /// silently adopted.
    pub fn matches(&self, other: &Self) -> bool {
        self.name == other.name && self.version == other.version && self.digest == other.digest
    }
}

/// Two sources defining the same skill name.
///
/// The conflict carries a stable id so a report, a log line and a test can name
/// the same decision without depending on scan order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillConflict {
    pub conflict_id: ContentHash,
    pub name: String,
    pub winner_source: SkillSource,
    pub loser_source: SkillSource,
    pub winner_digest: ContentHash,
    pub loser_digest: ContentHash,
}

impl SkillConflict {
    fn new(winner: &SkillCatalogEntry, loser: &SkillCatalogEntry) -> Result<Self, ExtensionError> {
        let conflict_id = ContentHash::from_canonical_json(&serde_json::json!({
            "domain": "skill-conflict.v1",
            "name": winner.name,
            "winner_source": winner.source.as_str(),
            "loser_source": loser.source.as_str(),
            "winner_digest": winner.digest.as_str(),
            "loser_digest": loser.digest.as_str(),
        }))
        .map_err(|error| ExtensionError::new(error.code(), error.to_string()))?;
        Ok(Self {
            conflict_id,
            name: winner.name.clone(),
            winner_source: winner.source,
            loser_source: loser.source,
            winner_digest: winner.digest.clone(),
            loser_digest: loser.digest.clone(),
        })
    }
}

/// The result of scanning trusted roots: metadata plus the decisions made.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillCatalog {
    entries: Vec<SkillCatalogEntry>,
    conflicts: Vec<SkillConflict>,
    catalog_digest: ContentHash,
    scanned_roots: Vec<TrustedSkillRoot>,
}

impl SkillCatalog {
    /// Scan every trusted root and compose the result by source precedence.
    ///
    /// Only the head of each document is read for metadata; the body is hashed
    /// in a stream and dropped. A missing root is reported as unavailable
    /// rather than silently contributing nothing.
    pub fn discover(roots: &[TrustedSkillRoot]) -> Result<Self, ExtensionError> {
        let mut candidates = Vec::new();
        for root in roots {
            for path in skill_files(&root.path)? {
                candidates.push(scan_entry(&path, root.source)?);
                if candidates.len() > MAX_SKILL_CATALOG_ENTRIES {
                    return Err(ExtensionError::new(
                        ErrorCode::FrameLimitExceeded,
                        format!(
                            "skill source holds more than {MAX_SKILL_CATALOG_ENTRIES} documents"
                        ),
                    ));
                }
            }
        }
        Self::compose(candidates, roots.to_vec())
    }

    fn compose(
        candidates: Vec<SkillCatalogEntry>,
        scanned_roots: Vec<TrustedSkillRoot>,
    ) -> Result<Self, ExtensionError> {
        let mut ordered = candidates;
        ordered.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then(left.source.precedence().cmp(&right.source.precedence()))
                .then(left.path.cmp(&right.path))
        });
        let mut entries: Vec<SkillCatalogEntry> = Vec::new();
        let mut conflicts = Vec::new();
        for candidate in ordered {
            match entries
                .iter_mut()
                .find(|entry| entry.name == candidate.name)
            {
                Some(existing) => {
                    let (winner, loser) =
                        if candidate.source.precedence() > existing.source.precedence() {
                            let winner = candidate.clone();
                            let loser = existing.clone();
                            *existing = candidate;
                            (winner, loser)
                        } else {
                            (existing.clone(), candidate)
                        };
                    conflicts.push(SkillConflict::new(&winner, &loser)?);
                }
                None => entries.push(candidate),
            }
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        conflicts.sort_by(|left, right| left.conflict_id.as_str().cmp(right.conflict_id.as_str()));
        let catalog_digest = ContentHash::from_canonical_json(&serde_json::json!({
            "domain": "skill-catalog.v1",
            "entries": entries
                .iter()
                .map(|entry| serde_json::json!({
                    "name": entry.name,
                    "version": entry.version,
                    "digest": entry.digest.as_str(),
                    "source": entry.source.as_str(),
                }))
                .collect::<Vec<_>>(),
        }))
        .map_err(|error| ExtensionError::new(error.code(), error.to_string()))?;
        Ok(Self {
            entries,
            conflicts,
            catalog_digest,
            scanned_roots,
        })
    }

    #[must_use]
    pub fn entries(&self) -> &[SkillCatalogEntry] {
        &self.entries
    }

    #[must_use]
    pub fn conflicts(&self) -> &[SkillConflict] {
        &self.conflicts
    }

    #[must_use]
    pub const fn catalog_digest(&self) -> &ContentHash {
        &self.catalog_digest
    }

    #[must_use]
    pub fn scanned_roots(&self) -> &[TrustedSkillRoot] {
        &self.scanned_roots
    }

    #[must_use]
    pub fn entry(&self, name: &str) -> Option<&SkillCatalogEntry> {
        self.entries.iter().find(|entry| entry.name == name)
    }

    /// Load one skill's content. This is the only place a skill body is read.
    ///
    /// `expected_digest` lets a caller pin the version it reviewed: a file that
    /// changed between the listing and the activation is refused instead of
    /// being adopted under the reviewed digest.
    pub fn activate(
        &self,
        name: &str,
        expected_digest: Option<&ContentHash>,
        activated_at_seq: u64,
    ) -> Result<SkillActivation, ExtensionError> {
        let entry = self.entry(name).ok_or_else(|| {
            ExtensionError::new(
                ErrorCode::SkillUnavailable,
                format!("no trusted skill named {name} is in this catalogue"),
            )
        })?;
        if let Some(expected) = expected_digest
            && expected != &entry.digest
        {
            return Err(ExtensionError::new(
                ErrorCode::SchemaVersionMismatch,
                format!(
                    "skill {name} is {} but the caller pinned {}",
                    entry.digest.as_str(),
                    expected.as_str()
                ),
            ));
        }
        let content = read_skill_body(&entry.path)?;
        let actual = ContentHash::from_bytes(content.as_bytes());
        if actual != entry.digest {
            return Err(ExtensionError::new(
                ErrorCode::SchemaVersionMismatch,
                format!(
                    "skill {name} changed on disk after it was listed: {} is not {}",
                    actual.as_str(),
                    entry.digest.as_str()
                ),
            ));
        }
        Ok(SkillActivation {
            catalog_digest: self.catalog_digest.clone(),
            entry: entry.clone(),
            content,
            activated_at_seq,
        })
    }
}

/// One skill whose content was loaded at an explicit activation boundary.
///
/// The content is pinned: a replay, a resume or a later step uses these bytes
/// even if the file changed or disappeared, and the pinned digest is what the
/// packet records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillActivation {
    pub catalog_digest: ContentHash,
    pub entry: SkillCatalogEntry,
    pub content: String,
    /// The event sequence at which this activation became effective.
    pub activated_at_seq: u64,
}

impl SkillActivation {
    #[must_use]
    pub fn content_digest(&self) -> ContentHash {
        ContentHash::from_bytes(self.content.as_bytes())
    }

    /// The pinned bytes, for persistence and replay. Never re-read from disk.
    #[must_use]
    pub fn replay_content(&self) -> &str {
        &self.content
    }

    /// Compare the pinned version with what is on disk now.
    ///
    /// A change is reported as a typed error; adopting it is the caller's
    /// decision at an admission boundary, never this call's.
    pub fn verify_against(&self, current: &SkillCatalogEntry) -> Result<(), ExtensionError> {
        if !self.entry.matches(current) {
            return Err(ExtensionError::new(
                ErrorCode::SchemaVersionMismatch,
                format!(
                    "skill {} moved from {} ({}) to {} ({})",
                    self.entry.name,
                    self.entry.version,
                    self.entry.digest.as_str(),
                    current.version,
                    current.digest.as_str()
                ),
            ));
        }
        Ok(())
    }

    /// The context block this activation contributes.
    ///
    /// It is a skill block on the skill channel, so it carries user authority
    /// and may be mandatory. It carries no grants: the requested tools and
    /// secrets stay in the catalogue metadata, and every one of them still has
    /// to pass the tool gate on each proposal.
    #[must_use]
    pub fn block(&self) -> ContextBlock {
        ContextBlock::mandatory(
            format!("skill:{}", self.entry.version_ref()),
            ContextBlockKind::Skill,
            self.content.clone(),
        )
        .on_channel(harness_session::ContextChannel::Skill)
    }

    /// The requests this skill made, as a report. Never a grant.
    #[must_use]
    pub fn requests(&self) -> serde_json::Value {
        serde_json::json!({
            "skill": self.entry.version_ref(),
            "requested_tools": self.entry.requested_tools,
            "requested_secrets": self.entry.requested_secrets,
        })
    }
}

/// Contributes one activated skill into the frozen context packet.
///
/// The contributor holds pinned content, so it is a pure function of the
/// activation: it never re-reads the file, which is what makes a replay after
/// compaction show the same skill version the packet was frozen with.
pub struct SkillContributor {
    activation: SkillActivation,
}

impl std::fmt::Debug for SkillContributor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SkillContributor")
            .field("skill", &self.activation.entry.version_ref())
            .field("digest", &self.activation.entry.digest.as_str())
            .finish()
    }
}

impl SkillContributor {
    #[must_use]
    pub fn new(activation: SkillActivation) -> Self {
        Self { activation }
    }

    #[must_use]
    pub const fn activation(&self) -> &SkillActivation {
        &self.activation
    }
}

impl ContextContributor for SkillContributor {
    fn contributor_id(&self) -> &str {
        &self.activation.entry.name
    }

    fn collect(
        &self,
        _scope: &ContributorScope<'_>,
        _token_limit: u64,
    ) -> Result<Vec<ContextBlock>, ContextError> {
        // The budget is the compiler's decision: returning the block and letting
        // the compiler rank and drop it keeps one place responsible for what a
        // request can hold.
        Ok(vec![self.activation.block()])
    }
}

/// List the skill documents under one root, in a deterministic order.
fn skill_files(root: &Path) -> Result<Vec<PathBuf>, ExtensionError> {
    if !root.is_dir() {
        return Err(ExtensionError::new(
            ErrorCode::SkillUnavailable,
            format!("skill source {} does not exist", root.display()),
        ));
    }
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(root).map_err(|error| {
        ExtensionError::new(
            ErrorCode::SkillUnavailable,
            format!("cannot read {}: {error}", root.display()),
        )
    })? {
        let entry = entry.map_err(|error| {
            ExtensionError::new(
                ErrorCode::SkillUnavailable,
                format!("cannot read a skill entry: {error}"),
            )
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| {
            ExtensionError::new(
                ErrorCode::SkillUnavailable,
                format!("cannot inspect skill entry {}: {error}", path.display()),
            )
        })?;
        if file_type.is_file()
            && path
                .extension()
                .is_some_and(|ext| ext == "md" || ext == "skill")
        {
            paths.push(path);
        } else if file_type.is_dir() {
            // Match the documented agent skill layout: one directory per
            // skill, with its metadata and body in SKILL.md. Do not recursively
            // treat arbitrary reference Markdown files as separate skills.
            let document = path.join("SKILL.md");
            if document.is_file() {
                paths.push(document);
            }
        }
    }
    paths.sort();
    Ok(paths)
}

/// Read only the head of one skill document and describe it.
///
/// Nothing here executes, interprets or retains the body: the head is parsed for
/// declared metadata and the whole file is streamed through the digest.
fn scan_entry(path: &Path, source: SkillSource) -> Result<SkillCatalogEntry, ExtensionError> {
    let metadata = std::fs::metadata(path).map_err(|error| {
        ExtensionError::new(
            ErrorCode::SkillUnavailable,
            format!("skill {} is unavailable: {error}", path.display()),
        )
    })?;
    if metadata.len() > MAX_SKILL_BYTES as u64 {
        return Err(ExtensionError::new(
            ErrorCode::FrameLimitExceeded,
            format!(
                "skill {} is {} bytes, over the {MAX_SKILL_BYTES} byte limit",
                path.display(),
                metadata.len()
            ),
        ));
    }
    let head = read_prefix(path, MAX_SKILL_HEAD_BYTES)?;
    let head_text = String::from_utf8_lossy(&head).into_owned();
    let mut file = std::fs::File::open(path).map_err(|error| {
        ExtensionError::new(
            ErrorCode::SkillUnavailable,
            format!("skill {} is unavailable: {error}", path.display()),
        )
    })?;
    let digest = ContentHash::from_reader(&mut file).map_err(|error| {
        ExtensionError::new(
            ErrorCode::SkillUnavailable,
            format!("skill {} cannot be hashed: {error}", path.display()),
        )
    })?;
    let name = front_matter(&head_text, "name").unwrap_or_else(|| skill_name_from_path(path));
    Ok(SkillCatalogEntry {
        skill_id: SkillId::generate(),
        version: front_matter(&head_text, "version").unwrap_or_else(|| "0".to_owned()),
        name,
        description: front_matter(&head_text, "description").unwrap_or_default(),
        digest,
        source,
        path: path.to_path_buf(),
        byte_len: metadata.len(),
        requested_tools: {
            let requested = front_matter_list(&head_text, "requested_tools");
            if requested.is_empty() {
                front_matter_list(&head_text, "tools")
            } else {
                requested
            }
        },
        requested_secrets: front_matter_list(&head_text, "secrets"),
    })
}

/// Read a bounded prefix without reading the rest of the file.
fn read_prefix(path: &Path, limit: usize) -> Result<Vec<u8>, ExtensionError> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).map_err(|error| {
        ExtensionError::new(
            ErrorCode::SkillUnavailable,
            format!("skill {} is unavailable: {error}", path.display()),
        )
    })?;
    let mut buffer = Vec::new();
    file.take(limit as u64)
        .read_to_end(&mut buffer)
        .map_err(|error| {
            ExtensionError::new(
                ErrorCode::SkillUnavailable,
                format!("skill {} is unreadable: {error}", path.display()),
            )
        })?;
    Ok(buffer)
}

/// Read one skill document in full, with the size bound applied first.
fn read_skill_body(path: &Path) -> Result<String, ExtensionError> {
    let metadata = std::fs::metadata(path).map_err(|error| {
        ExtensionError::new(
            ErrorCode::SkillUnavailable,
            format!("skill {} is unavailable: {error}", path.display()),
        )
    })?;
    if metadata.len() > MAX_SKILL_BYTES as u64 {
        return Err(ExtensionError::new(
            ErrorCode::FrameLimitExceeded,
            format!(
                "skill {} is {} bytes, over the {MAX_SKILL_BYTES} byte limit",
                path.display(),
                metadata.len()
            ),
        ));
    }
    let bytes = std::fs::read(path).map_err(|error| {
        ExtensionError::new(
            ErrorCode::SkillUnavailable,
            format!("skill {} is unavailable: {error}", path.display()),
        )
    })?;
    String::from_utf8(bytes).map_err(|_| {
        ExtensionError::new(
            ErrorCode::UnsupportedTextEncoding,
            format!("skill {} is not valid UTF-8", path.display()),
        )
    })
}
