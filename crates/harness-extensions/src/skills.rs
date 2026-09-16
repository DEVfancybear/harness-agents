//! Versioned filesystem skills and trusted profile composition.
//!
//! A skill is data: it declares an identity, a version, a digest and its
//! provenance, and it may *request* tools and secrets. It can never grant
//! itself authority. Discovery is lazy, content is bounded, and an update only
//! becomes visible at a recorded boundary.

use std::path::{Path, PathBuf};

use harness_types::{ContentHash, ErrorCode, SkillId};
use serde::{Deserialize, Serialize};

use crate::contracts::{ExtensionError, MAX_SKILL_BYTES};

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
        let name = path.file_stem().map_or_else(
            || "unnamed".to_owned(),
            |stem| stem.to_string_lossy().into_owned(),
        );
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
