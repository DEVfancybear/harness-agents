//! Authorized tool catalogue with deferred, bounded schema promotion.
//!
//! A catalogue answers two different questions, and conflating them is how a
//! discovery step quietly becomes an authorization step:
//!
//! * **What may be named?** Every tool the caller is authorized to see appears
//!   by name, source, revision and digest — and by nothing else.
//! * **What may be called, and with what shape?** A schema is *promoted* only on
//!   request, only for an entry the caller may see, and only against the exact
//!   catalogue digest it was requested from. It is then revalidated immediately
//!   before execution against the live catalogue and policy revision.
//!
//! A promoted schema is a description, never a permission. Revoking a grant or
//! changing a server's schema makes every previously promoted definition stale,
//! and a stale definition is refused rather than used.

use std::sync::Arc;

use harness_session::{
    ContextBlock, ContextBlockKind, ContextChannel, ContextContributor, ContextError,
    ContributorScope,
};
use harness_tools::EffectClass;
use harness_types::{ContentHash, ErrorCode};
use serde_json::Value;

use crate::contracts::ExtensionError;

/// Bound on one promoted schema. A schema is promoted into a request, so an
/// unbounded one would be an unbounded request.
pub const MAX_PROMOTED_SCHEMA_BYTES: usize = 16 * 1024;

/// Entries one catalogue may hold.
pub const MAX_CATALOG_ENTRIES: usize = 512;

/// Where one catalogued tool comes from.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CatalogSource {
    /// A built-in tool of this build.
    Builtin,
    /// A tool provided by a trusted external extension process.
    Extension { plugin_id: String },
    /// A tool discovered from an MCP server.
    Mcp { server: String },
}

impl CatalogSource {
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Builtin => "builtin".to_owned(),
            Self::Extension { plugin_id } => format!("extension:{plugin_id}"),
            Self::Mcp { server } => format!("mcp:{server}"),
        }
    }
}

/// One catalogued tool.
///
/// `schema` is `None` until the definition is promoted, which is what makes the
/// listing cheap and inert: a catalogue can describe a thousand tools without
/// carrying a thousand schemas into any request.
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogEntry {
    /// The name the model sees.
    pub id: String,
    /// The name the providing process expects.
    pub tool_name: String,
    pub source: CatalogSource,
    pub revision: u32,
    pub schema_digest: ContentHash,
    pub effect_class: EffectClass,
    /// Capabilities a caller must hold to see and to promote this entry.
    pub capabilities: Vec<String>,
    pub summary: String,
    schema: Option<Value>,
    /// Set when a revoke or a reload dropped this entry's definition. The name
    /// stays visible so a report can say *why* it is unusable; the definition
    /// can never be promoted again under this revision.
    revoked: bool,
}

impl CatalogEntry {
    /// Build one entry. The summary is set separately so the constructor stays
    /// within a readable argument count.
    pub fn new(
        id: impl Into<String>,
        tool_name: impl Into<String>,
        source: CatalogSource,
        revision: u32,
        schema: Value,
        effect_class: EffectClass,
        capabilities: Vec<String>,
    ) -> Result<Self, ExtensionError> {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(ExtensionError::new(
                ErrorCode::InvalidPayload,
                "a catalogue entry requires a non-empty id",
            ));
        }
        let schema_digest = ContentHash::from_canonical_json(&schema)
            .map_err(|error| ExtensionError::new(error.code(), error.to_string()))?;
        Ok(Self {
            id,
            tool_name: tool_name.into(),
            source,
            revision,
            schema_digest,
            effect_class,
            capabilities,
            summary: String::new(),
            schema: Some(schema),
            revoked: false,
        })
    }

    /// Describe the entry for a listing.
    #[must_use]
    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = summary.into();
        self
    }

    /// Whether a caller holding `held` may see this entry.
    ///
    /// An entry that requires nothing is visible to everyone; an entry that
    /// requires something is visible only to a caller that holds all of it.
    #[must_use]
    pub fn is_authorized(&self, held: &[String]) -> bool {
        self.capabilities
            .iter()
            .all(|required| held.iter().any(|capability| capability == required))
    }

    /// Whether a definition has been promoted.
    #[must_use]
    pub const fn is_promoted(&self) -> bool {
        self.schema.is_some()
    }

    /// Whether this entry's definition was dropped by a revoke or a reload.
    #[must_use]
    pub const fn is_revoked(&self) -> bool {
        self.revoked
    }
}

/// What a listing may reveal: identity and metadata, never a schema.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogNameView {
    pub id: String,
    pub source: String,
    pub revision: u32,
    pub schema_digest: String,
    pub effect_class: String,
    pub summary: String,
    pub promoted: bool,
    pub revoked: bool,
}

/// One tool whose definition was promoted for a specific catalogue revision.
#[derive(Clone, Debug, PartialEq)]
pub struct PromotedTool {
    pub catalog_digest: ContentHash,
    pub catalog_revision: u64,
    pub id: String,
    pub tool_name: String,
    pub source: CatalogSource,
    pub schema: Value,
    pub schema_digest: ContentHash,
    pub effect_class: EffectClass,
    /// The policy revision in force when this definition was promoted.
    pub policy_revision: u64,
}

impl PromotedTool {
    /// Re-check this definition immediately before execution.
    ///
    /// Both facts that made the promotion valid are re-read here: the catalogue
    /// the definition came from, and the policy revision it was promoted under.
    /// A change in either refuses the call, so a definition that was authorized
    /// at discovery time cannot be executed after the authority moved.
    pub fn revalidate(
        &self,
        current: &ToolCatalog,
        policy_revision: u64,
    ) -> Result<(), ExtensionError> {
        if current.catalog_digest != self.catalog_digest {
            return Err(ExtensionError::new(
                ErrorCode::SchemaVersionMismatch,
                format!(
                    "the catalogue for {} changed since its definition was promoted",
                    self.id
                ),
            ));
        }
        if policy_revision != self.policy_revision {
            return Err(ExtensionError::new(
                ErrorCode::PolicyDenied,
                format!(
                    "the policy revision for {} moved from {} to {policy_revision}",
                    self.id, self.policy_revision
                ),
            ));
        }
        let Some(entry) = current.entry(&self.id) else {
            return Err(ExtensionError::new(
                ErrorCode::PolicyDenied,
                format!("{} is no longer in the authorized catalogue", self.id),
            ));
        };
        if entry.revoked {
            return Err(ExtensionError::new(
                ErrorCode::PolicyDenied,
                format!("the definition for {} was revoked", self.id),
            ));
        }
        if entry.schema_digest != self.schema_digest {
            return Err(ExtensionError::new(
                ErrorCode::SchemaVersionMismatch,
                format!("the schema for {} changed after it was promoted", self.id),
            ));
        }
        Ok(())
    }
}

/// The authorized catalogue of one revision.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCatalog {
    revision: u64,
    catalog_digest: ContentHash,
    entries: Vec<CatalogEntry>,
}

impl ToolCatalog {
    /// Build a catalogue revision from its entries.
    pub fn build(revision: u64, entries: Vec<CatalogEntry>) -> Result<Self, ExtensionError> {
        if revision == 0 {
            return Err(ExtensionError::new(
                ErrorCode::InvalidPayload,
                "a catalogue revision must be positive",
            ));
        }
        if entries.len() > MAX_CATALOG_ENTRIES {
            return Err(ExtensionError::new(
                ErrorCode::FrameLimitExceeded,
                format!("a catalogue holds at most {MAX_CATALOG_ENTRIES} entries"),
            ));
        }
        let mut entries = entries;
        entries.sort_by(|left, right| left.id.cmp(&right.id));
        let catalog_digest = ContentHash::from_canonical_json(&serde_json::json!({
            "domain": "tool-catalog.v1",
            "revision": revision,
            "entries": entries
                .iter()
                .map(|entry| serde_json::json!({
                    "id": entry.id,
                    "source": entry.source.label(),
                    "revision": entry.revision,
                    "schema_digest": entry.schema_digest.as_str(),
                    "effect_class": entry.effect_class.as_str(),
                    "revoked": entry.revoked,
                }))
                .collect::<Vec<_>>(),
        }))
        .map_err(|error| ExtensionError::new(error.code(), error.to_string()))?;
        Ok(Self {
            revision,
            catalog_digest,
            entries,
        })
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub const fn catalog_digest(&self) -> &ContentHash {
        &self.catalog_digest
    }

    #[must_use]
    pub fn entries(&self) -> &[CatalogEntry] {
        &self.entries
    }

    #[must_use]
    pub fn entry(&self, id: &str) -> Option<&CatalogEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    /// Names and metadata the caller may see. Never a schema.
    #[must_use]
    pub fn list_authorized(&self, held: &[String]) -> Vec<CatalogNameView> {
        self.entries
            .iter()
            .filter(|entry| entry.is_authorized(held))
            .map(|entry| CatalogNameView {
                id: entry.id.clone(),
                source: entry.source.label(),
                revision: entry.revision,
                schema_digest: entry.schema_digest.as_str().to_owned(),
                effect_class: entry.effect_class.as_str().to_owned(),
                summary: entry.summary.clone(),
                promoted: entry.is_promoted(),
                revoked: entry.revoked,
            })
            .collect()
    }

    /// Promote one bounded definition for execution.
    ///
    /// `expected_catalog_digest` is the revision the caller searched against: a
    /// catalogue that moved between the search and the promotion is refused, so
    /// a definition is never promoted from a listing the caller did not see.
    pub fn promote(
        &self,
        id: &str,
        expected_catalog_digest: &ContentHash,
        policy_revision: u64,
        held: &[String],
    ) -> Result<PromotedTool, ExtensionError> {
        if expected_catalog_digest != &self.catalog_digest {
            return Err(ExtensionError::new(
                ErrorCode::SchemaVersionMismatch,
                format!("the catalogue moved since {id} was searched for"),
            ));
        }
        let entry = self.entry(id).ok_or_else(|| {
            ExtensionError::new(
                ErrorCode::PolicyDenied,
                format!("{id} is not in the authorized catalogue"),
            )
        })?;
        if !entry.is_authorized(held) {
            return Err(ExtensionError::new(
                ErrorCode::PolicyDenied,
                format!("{id} requires capabilities the caller does not hold"),
            ));
        }
        if entry.revoked {
            return Err(ExtensionError::new(
                ErrorCode::PolicyDenied,
                format!("the definition for {id} was revoked and cannot be promoted"),
            ));
        }
        let schema = entry.schema.clone().ok_or_else(|| {
            ExtensionError::new(
                ErrorCode::SchemaVersionMismatch,
                format!("{id} has no definition to promote"),
            )
        })?;
        let encoded = serde_json::to_vec(&schema).map_err(|_| {
            ExtensionError::new(ErrorCode::InvalidPayload, "schema is not serializable")
        })?;
        if encoded.len() > MAX_PROMOTED_SCHEMA_BYTES {
            return Err(ExtensionError::new(
                ErrorCode::FrameLimitExceeded,
                format!(
                    "the schema for {id} is {} bytes, over the {MAX_PROMOTED_SCHEMA_BYTES} byte limit",
                    encoded.len()
                ),
            ));
        }
        Ok(PromotedTool {
            catalog_digest: self.catalog_digest.clone(),
            catalog_revision: self.revision,
            id: entry.id.clone(),
            tool_name: entry.tool_name.clone(),
            source: entry.source.clone(),
            schema,
            schema_digest: entry.schema_digest.clone(),
            effect_class: entry.effect_class,
            policy_revision,
        })
    }

    /// Drop one entry's promoted definition, keeping its name visible.
    ///
    /// This is what a revoke or a reload does: the tool does not vanish from the
    /// catalogue mid-turn, but its definition can no longer be used and every
    /// definition promoted from the previous digest is refused.
    pub fn invalidate(&mut self, id: &str) -> bool {
        let digest_before = self.catalog_digest.clone();
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) else {
            return false;
        };
        entry.schema = None;
        entry.revoked = true;
        // The catalogue digest covers revocation, so a definition promoted
        // before the revoke is stale by digest as well as by flag.
        match Self::build(self.revision, self.entries.clone()) {
            Ok(rebuilt) => {
                self.catalog_digest = rebuilt.catalog_digest;
                debug_assert_ne!(digest_before, self.catalog_digest);
            }
            Err(_) => {
                // Rebuilding a catalogue that already existed cannot fail; if it
                // somehow does, the digest must not silently keep describing the
                // pre-revocation state.
                self.catalog_digest = ContentHash::from_bytes(b"catalog-revocation-unknown");
            }
        }
        true
    }

    /// Replace the catalogue with a new revision, invalidating every promoted
    /// definition by construction: the digest changes with the entries.
    pub fn rebuild(
        &mut self,
        revision: u64,
        entries: Vec<CatalogEntry>,
    ) -> Result<(), ExtensionError> {
        let rebuilt = Self::build(revision, entries)?;
        *self = rebuilt;
        Ok(())
    }

    /// A stable digest of what the caller may see, for a report.
    pub fn authorized_digest(&self, held: &[String]) -> Result<ContentHash, ExtensionError> {
        ContentHash::from_canonical_json(&serde_json::json!({
            "domain": "tool-catalog-view.v1",
            "catalog_digest": self.catalog_digest.as_str(),
            "visible": self
                .list_authorized(held)
                .iter()
                .map(|view| view.id.clone())
                .collect::<Vec<_>>(),
        }))
        .map_err(|error| ExtensionError::new(error.code(), error.to_string()))
    }
}

/// Contributes the authorized catalogue's *names* into a context packet.
///
/// The block is a reference on the reference channel, so it is never mandatory:
/// a catalogue listing is material the model may consult, not an instruction it
/// must obey. Full definitions stay out of the packet entirely — a tool is
/// promoted on demand, and promoting it is not a grant.
pub struct ToolContributor {
    catalog: ToolCatalog,
    held: Vec<String>,
}

impl std::fmt::Debug for ToolContributor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolContributor")
            .field("catalog_revision", &self.catalog.revision())
            .field("capabilities", &self.held.len())
            .finish()
    }
}

impl ToolContributor {
    #[must_use]
    pub fn new(catalog: ToolCatalog, held: Vec<String>) -> Self {
        Self { catalog, held }
    }

    #[must_use]
    pub const fn catalog(&self) -> &ToolCatalog {
        &self.catalog
    }
}

impl ContextContributor for ToolContributor {
    fn contributor_id(&self) -> &'static str {
        "tool-catalog"
    }

    fn collect(
        &self,
        _scope: &ContributorScope<'_>,
        _token_limit: u64,
    ) -> Result<Vec<ContextBlock>, ContextError> {
        let visible = self.catalog.list_authorized(&self.held);
        if visible.is_empty() {
            return Ok(Vec::new());
        }
        let lines = visible
            .iter()
            .map(|view| format!("{} ({}) - {}", view.id, view.source, view.summary))
            .collect::<Vec<_>>()
            .join("\n");
        let text = format!(
            "catalogue revision {} ({})\n{lines}",
            self.catalog.revision(),
            self.catalog.catalog_digest().as_str()
        );
        Ok(vec![
            ContextBlock::optional(
                format!("tool-catalog@{}", self.catalog.revision()),
                ContextBlockKind::Optional,
                text,
                0,
            )
            .on_channel(ContextChannel::Reference),
        ])
    }
}

/// Build a catalogue from the built-in tool descriptors plus externally provided
/// entries, so one revision covers everything a caller may name.
pub fn catalog_from_descriptors(
    revision: u64,
    external: Vec<CatalogEntry>,
) -> Result<ToolCatalog, ExtensionError> {
    let mut entries = harness_tools::coding_tool_descriptors()
        .into_iter()
        .map(|descriptor| {
            let schema = harness_tools::coding_tool_schemas()
                .into_iter()
                .find(|schema| {
                    schema
                        .get("function")
                        .and_then(|function| function.get("name"))
                        .and_then(Value::as_str)
                        == Some(descriptor.id.as_str())
                })
                .unwrap_or_else(|| serde_json::json!({"type": "object"}));
            CatalogEntry {
                id: descriptor.id.clone(),
                tool_name: descriptor.id,
                source: CatalogSource::Builtin,
                revision: descriptor.revision,
                schema_digest: ContentHash::from_canonical_json(&schema)
                    .unwrap_or_else(|_| ContentHash::from_bytes(b"builtin")),
                effect_class: descriptor.effect_class,
                capabilities: descriptor.capabilities,
                summary: "built-in coding tool".to_owned(),
                schema: Some(schema),
                revoked: false,
            }
        })
        .collect::<Vec<_>>();
    entries.extend(external);
    ToolCatalog::build(revision, entries)
}

/// Wrap one catalogue for shared ownership by the host.
#[must_use]
pub fn shared_catalog(catalog: ToolCatalog) -> Arc<ToolCatalog> {
    Arc::new(catalog)
}
