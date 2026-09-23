use harness_types::{
    ContentHash, ContextPacket, ContextPacketId, MemoryAssetId, MemoryVersionRef,
    P0_SCHEMA_VERSION, SessionId, SourceAuthority, SourceRef, TaskId,
};
use serde::{Deserialize, Serialize};

use crate::RecoveryView;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextBlockKind {
    SystemPolicy,
    ProjectRule,
    Instruction,
    WorkingState,
    RecentTail,
    Memory,
    Optional,
    /// Text authored in a local skill the user explicitly activated.
    Skill,
}

/// Where a block came from, and therefore what it is allowed to be.
///
/// A kind says what a block *is*; a channel says who put it there. The
/// distinction is what keeps a summary or a note from arriving as a host
/// instruction: those channels can carry text into the request, but they can
/// never make it mandatory, and they can never widen what the run may do.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextChannel {
    Policy,
    ProjectRule,
    Instruction,
    State,
    Tail,
    Summary,
    Memory,
    Reference,
    Note,
    /// An activated local skill. The activation is the user's act, so the text
    /// may be mandatory; what the skill *requests* is still only a proposal.
    Skill,
}

impl ContextChannel {
    /// The channel a block of this kind belongs to by default.
    #[must_use]
    pub const fn from_kind(kind: ContextBlockKind) -> Self {
        match kind {
            ContextBlockKind::SystemPolicy => Self::Policy,
            ContextBlockKind::ProjectRule => Self::ProjectRule,
            ContextBlockKind::Instruction => Self::Instruction,
            ContextBlockKind::WorkingState => Self::State,
            ContextBlockKind::RecentTail => Self::Tail,
            ContextBlockKind::Memory => Self::Memory,
            ContextBlockKind::Optional => Self::Reference,
            ContextBlockKind::Skill => Self::Skill,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Policy => "policy",
            Self::ProjectRule => "project_rule",
            Self::Instruction => "instruction",
            Self::State => "state",
            Self::Tail => "tail",
            Self::Summary => "summary",
            Self::Memory => "memory",
            Self::Reference => "reference",
            Self::Note => "note",
            Self::Skill => "skill",
        }
    }

    /// Whether a block on this channel may be mandatory.
    ///
    /// A summary is the host's own compaction output: it replaces conversation
    /// history, so it is carried as mandatory content. Everything else that is
    /// *derived* — a note, a retrieved memory, an optional reference — may be
    /// read, never obeyed: a note whose text claims new powers must not reach
    /// the model wearing the host's authority.
    ///
    /// A skill is not derived: it is an authored document the user activated by
    /// name, which is why it sits with project rules rather than with notes.
    /// Being mandatory here only means the text is *sent*; it never means the
    /// skill may do anything, because tool and secret requests still have to
    /// pass the tool gate on every proposal.
    #[must_use]
    pub const fn may_be_mandatory(self) -> bool {
        matches!(
            self,
            Self::Policy
                | Self::ProjectRule
                | Self::Instruction
                | Self::State
                | Self::Tail
                | Self::Summary
                | Self::Skill
        )
    }

    /// The authority a block on this channel carries when its producer names none.
    #[must_use]
    pub const fn default_authority(self) -> SourceAuthority {
        match self {
            Self::Policy | Self::ProjectRule => SourceAuthority::HostPolicy,
            Self::Instruction | Self::Skill => SourceAuthority::User,
            Self::State | Self::Tail | Self::Memory => SourceAuthority::RuntimeObserved,
            Self::Summary | Self::Note | Self::Reference => SourceAuthority::ModelProposed,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextBlock {
    pub id: String,
    pub kind: ContextBlockKind,
    pub text: String,
    pub mandatory: bool,
    pub relevance: i32,
    /// Who contributed this block.
    pub channel: ContextChannel,
    /// The authority class the block's producer claims.
    pub authority: SourceAuthority,
    /// Durable sources this text was derived from.
    pub provenance: Vec<SourceRef>,
    /// Block ids this block replaces; a superseded block is not sent at all.
    pub supersedes: Vec<String>,
    /// Digest of `text`, so a freeze can prove what was compiled.
    pub digest: ContentHash,
}

impl ContextBlock {
    #[must_use]
    pub fn mandatory(
        id: impl Into<String>,
        kind: ContextBlockKind,
        text: impl Into<String>,
    ) -> Self {
        let text = text.into();
        let channel = ContextChannel::from_kind(kind);
        Self {
            id: id.into(),
            kind,
            digest: ContentHash::from_bytes(text.as_bytes()),
            text,
            mandatory: true,
            relevance: 0,
            channel,
            authority: channel.default_authority(),
            provenance: Vec::new(),
            supersedes: Vec::new(),
        }
    }
    #[must_use]
    pub fn optional(
        id: impl Into<String>,
        kind: ContextBlockKind,
        text: impl Into<String>,
        relevance: i32,
    ) -> Self {
        let text = text.into();
        let channel = ContextChannel::from_kind(kind);
        Self {
            id: id.into(),
            kind,
            digest: ContentHash::from_bytes(text.as_bytes()),
            text,
            mandatory: false,
            relevance,
            channel,
            authority: channel.default_authority(),
            provenance: Vec::new(),
            supersedes: Vec::new(),
        }
    }

    /// Put this block on a channel other than the one its kind implies.
    ///
    /// This is how a contributor declares that its text is derived: the channel
    /// decides whether the block may be mandatory and what authority it holds.
    #[must_use]
    pub fn on_channel(mut self, channel: ContextChannel) -> Self {
        self.channel = channel;
        self.authority = channel.default_authority();
        self
    }

    #[must_use]
    pub fn with_authority(mut self, authority: SourceAuthority) -> Self {
        self.authority = authority;
        self
    }

    #[must_use]
    pub fn with_provenance(mut self, provenance: Vec<SourceRef>) -> Self {
        self.provenance = provenance;
        self
    }

    #[must_use]
    pub fn superseding(mut self, superseded: impl IntoIterator<Item = String>) -> Self {
        self.supersedes = superseded.into_iter().collect();
        self
    }
}

/// The host facts one compiled packet was built from.
///
/// A packet is only reproducible if the configuration, the model and the tool
/// definitions that shaped it are recorded with it: the same blocks compiled
/// under a different model or a different tool set are a different request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextManifest {
    pub rendering_version: u16,
    pub config_revision: u64,
    pub model_id: String,
    /// Digest of the capability record the model was called under, so a change
    /// in what the provider supports is visible in the frozen packet.
    pub model_capabilities_digest: ContentHash,
    pub tool_definition_digests: Vec<String>,
    pub source_revision: u64,
    pub channel_digest: ContentHash,
}

impl ContextManifest {
    pub fn validate(&self) -> Result<(), ContextError> {
        if self.rendering_version == 0 {
            return Err(ContextError::new(
                harness_types::ErrorCode::UnsupportedSchemaVersion,
                "context manifest rendering version must start at 1",
            ));
        }
        if self.config_revision == 0 {
            return Err(ContextError::new(
                harness_types::ErrorCode::InvalidPayload,
                "context manifest config revision must be positive",
            ));
        }
        if self.model_id.trim().is_empty() {
            return Err(ContextError::new(
                harness_types::ErrorCode::InvalidPayload,
                "context manifest must name the model it was built for",
            ));
        }
        Ok(())
    }
}

/// What the caller knows about the model, config and tools behind one request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextManifestInputs {
    pub config_revision: u64,
    pub model_id: String,
    pub model_capabilities_digest: ContentHash,
    pub tool_definition_digests: Vec<String>,
}

impl Default for ContextManifestInputs {
    fn default() -> Self {
        Self {
            config_revision: 1,
            model_id: "unspecified".to_owned(),
            model_capabilities_digest: ContentHash::from_bytes(b"unspecified"),
            tool_definition_digests: Vec::new(),
        }
    }
}

pub struct ContextBuildRequest {
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub checkpoint_id: String,
    pub through_event_seq: u64,
    pub recovery: RecoveryView,
    pub project_rules: Vec<ContextBlock>,
    pub optional_blocks: Vec<ContextBlock>,
    pub recent_tail: Vec<ContextBlock>,
    pub context_window_tokens: u64,
    pub output_reservation_tokens: u64,
    pub protocol_overhead_tokens: u64,
    pub safety_margin_tokens: u64,
    pub optional_token_budget: u64,
    pub memory_versions: Vec<MemoryVersionRef>,
    /// Bytes the request carries besides the compiled blocks: the system
    /// policy, the tool definitions, the user message and any images.
    ///
    /// The budget has to cover the whole serialized request, not just the
    /// blocks this compiler writes, or a schema-heavy tool set would overflow a
    /// window the estimator called fine.
    pub fixed_request_bytes: usize,
    pub manifest: ContextManifestInputs,
}

#[derive(Clone, Debug)]
pub struct ContextBuildResult {
    pub packet: ContextPacket,
    pub manifest: ContextManifest,
    pub mandatory_block_ids: Vec<String>,
    pub optional_block_ids: Vec<String>,
    pub omitted_optional: Vec<String>,
    /// Blocks that were replaced by a block claiming to supersede them.
    pub superseded_block_ids: Vec<String>,
    pub mandatory_tokens: u64,
    pub optional_tokens: u64,
    pub degradation: Option<String>,
    /// Per-block accounting for inspection surfaces such as `/context`.
    pub block_usage: Vec<ContextBlockUsage>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextBlockUsage {
    pub block_id: String,
    pub channel: ContextChannel,
    pub token_estimate: u64,
    pub included: bool,
    pub drop_reason: Option<String>,
}

#[derive(Clone, Debug, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ContextError {
    code: harness_types::ErrorCode,
    message: String,
}

impl ContextError {
    fn new(code: harness_types::ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    #[must_use]
    pub const fn code(&self) -> harness_types::ErrorCode {
        self.code
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ContextBuilder;

/// What a contributor is allowed to know about the run it contributes to.
///
/// It is deliberately a read-only view: a contributor receives identity and
/// revisions, never a store handle, a transaction or a mutable packet. A
/// contributor that cannot reach the packet cannot inject into a frozen step.
pub struct ContributorScope<'a> {
    pub session_id: SessionId,
    pub task_id: TaskId,
    /// The event sequence the packet will be frozen at.
    pub source_revision: u64,
    pub config_revision: u64,
    /// Workspace root, when the caller knows it. Contributors must not read
    /// outside it, and a contributor that needs no filesystem gets `None`.
    pub workspace_root: Option<&'a std::path::Path>,
}

/// A read-only producer of proposed context blocks.
///
/// The port exists so M6/M7/M8 can add sources — skills, memory, child results —
/// without any of them owning the compiler. Everything a contributor returns
/// goes through the same admission the caller's own blocks do, which is what
/// keeps a contributor from widening its own authority:
///
/// * a block on a channel that cannot be mandatory is forced optional;
/// * a block that is superseded is dropped;
/// * the surviving blocks are hashed into the frozen manifest's channel digest.
pub trait ContextContributor: Send + Sync {
    /// Stable identity, used in diagnostics and in the contribution report.
    fn contributor_id(&self) -> &str;

    /// Propose blocks for one compilation.
    ///
    /// `token_limit` is the budget this contributor may spend; returning more
    /// than the budget is not an error, the compiler ranks and drops the
    /// overflow like any other optional block. A contributor that cannot read
    /// its source must say so with a typed error rather than return an empty
    /// vector, because "no content" and "content unavailable" are different
    /// facts for the packet's provenance.
    fn collect(
        &self,
        scope: &ContributorScope<'_>,
        token_limit: u64,
    ) -> Result<Vec<ContextBlock>, ContextError>;
}

impl ContextBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Compile a packet with no contributors. Kept as the default entry point so
    /// callers that have no extra sources do not carry the port.
    pub fn build(&self, request: ContextBuildRequest) -> Result<ContextBuildResult, ContextError> {
        self.build_with_contributors(request, &[])
    }

    /// Compile a packet, letting each contributor propose optional blocks.
    ///
    /// A contributor failure is a typed compile failure: a source that is
    /// supposed to be present but cannot be read must not silently compile into
    /// a packet that looks complete.
    pub fn build_with_contributors(
        &self,
        request: ContextBuildRequest,
        contributors: &[std::sync::Arc<dyn ContextContributor>],
    ) -> Result<ContextBuildResult, ContextError> {
        let mut request = request;
        if !contributors.is_empty() {
            let scope = ContributorScope {
                session_id: request.session_id.clone(),
                task_id: request.task_id.clone(),
                source_revision: request.through_event_seq,
                config_revision: request.manifest.config_revision,
                workspace_root: None,
            };
            for contributor in contributors {
                let proposed = contributor.collect(&scope, request.optional_token_budget)?;
                request.optional_blocks.extend(proposed);
            }
        }
        Self::build_inner(request)
    }

    #[allow(clippy::too_many_lines)]
    fn build_inner(request: ContextBuildRequest) -> Result<ContextBuildResult, ContextError> {
        if request.context_window_tokens
            <= request
                .output_reservation_tokens
                .saturating_add(request.protocol_overhead_tokens)
                .saturating_add(request.safety_margin_tokens)
        {
            return Err(ContextError::new(
                harness_types::ErrorCode::MandatoryContextOverflow,
                "context window leaves no space for mandatory state",
            ));
        }
        let available = request.context_window_tokens
            - request.output_reservation_tokens
            - request.protocol_overhead_tokens
            - request.safety_margin_tokens;
        let fixed_bytes = request.fixed_request_bytes;
        let mut mandatory = Vec::<ContextBlock>::new();
        let mut source_manifest = vec![request.recovery.working_state.objective_ref.clone()];
        source_manifest.extend(
            request
                .recovery
                .working_state
                .acceptance_criteria_refs
                .iter()
                .cloned(),
        );
        source_manifest.extend(request.recovery.working_state.decision_refs.iter().cloned());
        // The system policy is not a block here: the runtime already sends it as the
        // conversation's system message, and repeating it inside the request text
        // gave the model two copies of one policy and pushed its own instruction into
        // a wall of quoted blocks.
        for mut block in request.project_rules {
            block.mandatory = block.mandatory && block.channel.may_be_mandatory();
            mandatory.push(block);
        }
        for (index, text) in request.recovery.instruction_texts.iter().enumerate() {
            mandatory.push(ContextBlock::mandatory(
                format!("instruction-{index}"),
                ContextBlockKind::Instruction,
                text.clone(),
            ));
        }
        let state_text = serde_json::to_string(&request.recovery.working_state).map_err(|_| {
            ContextError::new(
                harness_types::ErrorCode::InvalidPayload,
                "working state cannot be rendered",
            )
        })?;
        mandatory.push(ContextBlock::mandatory(
            "working-state",
            ContextBlockKind::WorkingState,
            state_text,
        ));
        for block in request.recent_tail {
            let mut block = block;
            block.mandatory = block.mandatory && block.channel.may_be_mandatory();
            mandatory.push(block);
        }

        // An effective instruction replaces the one it supersedes: sending both
        // would let a corrected instruction compete with the correction.
        let superseded_ids = mandatory
            .iter()
            .chain(request.optional_blocks.iter())
            .flat_map(|block| block.supersedes.iter().cloned())
            .collect::<std::collections::BTreeSet<_>>();
        let mut superseded_block_ids = Vec::new();
        let mut block_usage = Vec::new();
        mandatory.retain(|block| {
            let replaced = superseded_ids.contains(&block.id);
            if replaced {
                superseded_block_ids.push(block.id.clone());
                block_usage.push(ContextBlockUsage {
                    block_id: block.id.clone(),
                    channel: block.channel,
                    token_estimate: estimate_bytes(render_block(block).len()),
                    included: false,
                    drop_reason: Some("superseded by a newer block".to_owned()),
                });
            }
            !replaced
        });
        for block in &mandatory {
            block_usage.push(ContextBlockUsage {
                block_id: block.id.clone(),
                channel: block.channel,
                token_estimate: estimate_bytes(render_block(block).len()),
                included: true,
                drop_reason: None,
            });
        }

        let mut content = mandatory
            .iter()
            .map(render_block)
            .collect::<Vec<_>>()
            .join("\n\n");
        let mandatory_tokens = estimate_bytes(fixed_bytes.saturating_add(content.len()));
        if mandatory_tokens > available {
            return Err(ContextError::new(
                harness_types::ErrorCode::MandatoryContextOverflow,
                format!(
                    "mandatory context requires {mandatory_tokens} tokens but only {available} are available"
                ),
            ));
        }

        let mut optional = request.optional_blocks;
        // A derived block is optional whatever its producer claimed, and a block
        // that was superseded is not sent at all.
        for block in &mut optional {
            if !block.channel.may_be_mandatory() {
                block.mandatory = false;
            }
        }
        optional.retain(|block| {
            if superseded_ids.contains(&block.id) {
                superseded_block_ids.push(block.id.clone());
                block_usage.push(ContextBlockUsage {
                    block_id: block.id.clone(),
                    channel: block.channel,
                    token_estimate: estimate_bytes(render_block(block).len()),
                    included: false,
                    drop_reason: Some("superseded by a newer block".to_owned()),
                });
                false
            } else {
                true
            }
        });
        optional.sort_by(|left, right| {
            right
                .relevance
                .cmp(&left.relevance)
                .then_with(|| left.id.cmp(&right.id))
        });
        let mut optional_tokens: u64 = 0;
        let mut optional_kept = Vec::new();
        let mut omitted_optional = Vec::new();
        let optional_budget = request
            .optional_token_budget
            .min(available - mandatory_tokens);
        for block in optional {
            let rendered = render_block(&block);
            let candidate_bytes = content
                .len()
                .saturating_add(2)
                .saturating_add(rendered.len());
            let candidate_optional_tokens =
                estimate_bytes(fixed_bytes.saturating_add(candidate_bytes))
                    .saturating_sub(mandatory_tokens);
            if candidate_optional_tokens <= optional_budget {
                optional_tokens = candidate_optional_tokens;
                content.push_str("\n\n");
                content.push_str(&rendered);
                block_usage.push(ContextBlockUsage {
                    block_id: block.id.clone(),
                    channel: block.channel,
                    token_estimate: estimate_bytes(rendered.len()),
                    included: true,
                    drop_reason: None,
                });
                optional_kept.push(block);
            } else {
                block_usage.push(ContextBlockUsage {
                    block_id: block.id.clone(),
                    channel: block.channel,
                    token_estimate: estimate_bytes(rendered.len()),
                    included: false,
                    drop_reason: Some("optional token budget".to_owned()),
                });
                omitted_optional.push(block.id);
            }
        }
        let channel_digest = ContentHash::from_canonical_json(&serde_json::json!({
            "mandatory": mandatory
                .iter()
                .map(|block| serde_json::json!({
                    "id": block.id,
                    "channel": block.channel.as_str(),
                    "authority": block.authority,
                    "digest": block.digest,
                    "provenance": block.provenance,
                }))
                .collect::<Vec<_>>(),
            "optional": optional_kept
                .iter()
                .map(|block| serde_json::json!({
                    "id": block.id,
                    "channel": block.channel.as_str(),
                    "authority": block.authority,
                    "digest": block.digest,
                }))
                .collect::<Vec<_>>(),
            "superseded": superseded_block_ids,
        }))
        .map_err(|error| ContextError::new(error.code(), error.to_string()))?;
        let manifest = ContextManifest {
            rendering_version: RENDERING_VERSION,
            config_revision: request.manifest.config_revision,
            model_id: request.manifest.model_id.clone(),
            model_capabilities_digest: request.manifest.model_capabilities_digest.clone(),
            tool_definition_digests: request.manifest.tool_definition_digests.clone(),
            source_revision: request.through_event_seq,
            channel_digest,
        };
        manifest.validate()?;
        let packet = ContextPacket {
            schema_version: P0_SCHEMA_VERSION,
            packet_id: ContextPacketId::generate(),
            session_id: request.session_id,
            task_id: request.task_id,
            checkpoint_id: request.checkpoint_id,
            through_event_seq: request.through_event_seq,
            memory_versions: request
                .memory_versions
                .into_iter()
                .filter(|reference| {
                    let id = format!("{}@{}", reference.memory_asset_id, reference.version);
                    optional_kept
                        .iter()
                        .any(|block| block.kind == ContextBlockKind::Memory && block.id == id)
                })
                .collect(),
            rendering_version: RENDERING_VERSION,
            token_estimate: estimate_bytes(fixed_bytes.saturating_add(content.len())),
            content_hash: ContentHash::from_bytes(content.as_bytes()),
            content,
            source_manifest,
        };
        packet.validate().map_err(|error| {
            ContextError::new(error.code(), format!("context packet is invalid: {error}"))
        })?;
        block_usage.sort_by(|left, right| left.block_id.cmp(&right.block_id));
        let degradation =
            (!omitted_optional.is_empty()).then(|| "optional_context_omitted".to_owned());
        Ok(ContextBuildResult {
            packet,
            manifest,
            mandatory_block_ids: mandatory.iter().map(|block| block.id.clone()).collect(),
            optional_block_ids: optional_kept.iter().map(|block| block.id.clone()).collect(),
            omitted_optional,
            superseded_block_ids,
            mandatory_tokens,
            optional_tokens,
            degradation,
            block_usage,
        })
    }
}

/// Version of the rendered packet text.
///
/// 2 drops the duplicated system-policy block and labels every block in words, so the
/// user's own instruction reads as an instruction rather than as a quoted tag. A
/// packet rendered by an older version is not comparable byte-for-byte with this one.
pub(crate) const RENDERING_VERSION: u16 = 2;

impl ContextBlock {
    /// The words the model reads before the block's text.
    ///
    /// A block id is provenance for inspection, not something a model can use, and the
    /// old `[kind:id]` header made the request itself look like metadata. Blocks whose
    /// identity is the point — a memory version, a project rule, an optional reference —
    /// keep it; the rest say plainly what they are.
    fn label(&self) -> String {
        match self.channel {
            ContextChannel::Summary => "earlier context summary".to_owned(),
            ContextChannel::Note => format!("note {}", self.id),
            // A skill block names the skill and the version it was pinned at, so
            // the model can tell an activated document from the user's own words.
            ContextChannel::Skill => format!("activated skill {}", self.id),
            _ => match self.kind {
                ContextBlockKind::SystemPolicy => "system policy".to_owned(),
                ContextBlockKind::Instruction => "user instruction".to_owned(),
                ContextBlockKind::WorkingState => "working state".to_owned(),
                ContextBlockKind::RecentTail => "earlier context".to_owned(),
                ContextBlockKind::ProjectRule => format!("project rule {}", self.id),
                ContextBlockKind::Memory => format!("memory {}", self.id),
                ContextBlockKind::Optional => format!("reference {}", self.id),
                ContextBlockKind::Skill => format!("activated skill {}", self.id),
            },
        }
    }
}

fn render_block(block: &ContextBlock) -> String {
    format!("[{}]\n{}", block.label(), block.text)
}

fn estimate_bytes(bytes: usize) -> u64 {
    u64::try_from(bytes.saturating_add(3) / 4)
        .unwrap_or(u64::MAX)
        .max(1)
}

#[allow(dead_code)]
fn _memory_marker(id: MemoryAssetId, version: u64) -> MemoryVersionRef {
    MemoryVersionRef {
        memory_asset_id: id,
        version,
    }
}

#[cfg(test)]
mod tests {
    use super::{ContextBlock, ContextBlockKind, RENDERING_VERSION, render_block};

    /// The measured failure: the model was sent `[instruction:instruction-0]` and told
    /// the user its instruction "isn't shown here", because the request looked like a
    /// quoted tag rather than the thing it had to do.
    #[test]
    fn a_rendered_block_names_what_it_is_instead_of_quoting_an_internal_id() {
        let instruction = ContextBlock::mandatory(
            "instruction-0",
            ContextBlockKind::Instruction,
            "sửa lỗi parser",
        );
        let rendered = render_block(&instruction);
        assert_eq!(rendered, "[user instruction]\nsửa lỗi parser");
        assert!(
            !rendered.contains("instruction-0"),
            "an internal id is provenance, not something the model can use: {rendered}"
        );
        assert_eq!(render_block(&instruction), render_block(&instruction));

        // Identity that carries meaning is kept: a memory block must show the version
        // it came from, and a project rule must stay distinguishable.
        let memory = ContextBlock::optional(
            "memory_asset_01a0@2",
            ContextBlockKind::Memory,
            "uses cargo test",
            10,
        );
        assert!(render_block(&memory).contains("memory_asset_01a0@2"));
        let rule = ContextBlock::mandatory("rule-no-api", ContextBlockKind::ProjectRule, "keep");
        assert!(render_block(&rule).contains("project rule rule-no-api"));
        assert_eq!(RENDERING_VERSION, 2);
    }
}
