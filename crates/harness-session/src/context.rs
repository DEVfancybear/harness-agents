use harness_types::{
    ContentHash, ContextPacket, ContextPacketId, MemoryVersionRef, P0_SCHEMA_VERSION, SessionId,
    SourceAuthority, SourceRef, TaskId,
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
    /// The most tokens this block may take in the request. A longer block is
    /// cut at a line boundary, with a note saying so, before it is admitted:
    /// an optional one competes for the budget at its cut size, and a
    /// mandatory one is sent cut rather than overflowing the window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// Where the part a cut leaves out can be read, named in the cut's note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rest_at: Option<String>,
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
            max_tokens: None,
            rest_at: None,
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
            max_tokens: None,
            rest_at: None,
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
    pub fn superseding(mut self, superseded: impl IntoIterator<Item = String>) -> Self {
        self.supersedes = superseded.into_iter().collect();
        self
    }

    /// Bound the block to `tokens` in the request; see [`Self::max_tokens`].
    #[must_use]
    pub fn with_max_tokens(mut self, tokens: u64) -> Self {
        self.max_tokens = Some(tokens);
        self
    }

    /// Name where the rest of a cut block can be read; see [`Self::rest_at`].
    #[must_use]
    pub fn with_rest_at(mut self, location: impl Into<String>) -> Self {
        self.rest_at = Some(location.into());
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
    /// The user messages the conversation already sends before this packet:
    /// the packets of its earlier turns, as they were sent.
    ///
    /// A block found word for word in one of them is not sent again - an
    /// activated skill, the project rules, a summary, a state that did not
    /// change - as prime-agent sends a skill once, where it was loaded. Every
    /// packet used to repeat them at the end of the request, the one part a
    /// provider never has cached, and an earlier turn replayed as its bare
    /// question made the request differ from the one before it at that turn,
    /// so nothing after it was cached either. The user's instruction is
    /// always sent.
    pub sent_earlier: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ContextBuildResult {
    pub packet: ContextPacket,
    pub manifest: ContextManifest,
    pub mandatory_block_ids: Vec<String>,
    pub optional_block_ids: Vec<String>,
    pub omitted_optional: Vec<String>,
    /// Blocks sent cut to their [`ContextBlock::max_tokens`].
    pub cut_block_ids: Vec<String>,
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
    /// The block's size before it was cut to its cap, when it was.
    pub cut_from_tokens: Option<u64>,
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
            message: harness_types::without_code_prefix(code, message.into()),
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
        // Each mandatory block with whether it goes ahead of the optional ones:
        // the project's rules and the host's facts do, and the user's
        // instruction, the working state and the tail come after them, so the
        // request still ends with what this turn is about.
        let mut mandatory = Vec::<(ContextBlock, bool)>::new();
        let mut optional = request.optional_blocks;
        // The system policy is not a block here: the runtime already sends it as the
        // conversation's system message, and repeating it inside the request text
        // gave the model two copies of one policy and pushed its own instruction into
        // a wall of quoted blocks.
        //
        // A host block that is not mandatory - its producer said so, or its
        // channel cannot be - competes for the optional budget by relevance
        // like any optional block. It used to be sent whatever its size, as if
        // it were mandatory, so the budget and the ranking never ran.
        for mut block in request.project_rules {
            block.mandatory = block.mandatory && block.channel.may_be_mandatory();
            if block.mandatory {
                mandatory.push((block, true));
            } else {
                optional.push(block);
            }
        }
        for (index, text) in request.recovery.instruction_texts.iter().enumerate() {
            mandatory.push((
                ContextBlock::mandatory(
                    format!("instruction-{index}"),
                    ContextBlockKind::Instruction,
                    text.clone(),
                ),
                false,
            ));
        }
        let state_text = render_working_state(&request.recovery.working_state);
        if !state_text.is_empty() {
            mandatory.push((
                ContextBlock::mandatory(
                    "working-state",
                    ContextBlockKind::WorkingState,
                    state_text,
                ),
                false,
            ));
        }
        for block in request.recent_tail {
            let mut block = block;
            block.mandatory = block.mandatory && block.channel.may_be_mandatory();
            mandatory.push((block, false));
        }
        // A block over its cap is cut before anything is admitted: an optional
        // one competes at its cut size, and a mandatory one is sent cut
        // instead of overflowing the window.
        let mut cuts = std::collections::BTreeMap::<String, u64>::new();
        for block in mandatory
            .iter_mut()
            .map(|(block, _)| block)
            .chain(optional.iter_mut())
        {
            if let Some(before) = cut_to_cap(block) {
                cuts.insert(block.id.clone(), before);
            }
        }

        // An effective instruction replaces the one it supersedes: sending both
        // would let a corrected instruction compete with the correction.
        //
        // Only a block that is certain to be sent retires another one here: an
        // optional block may yet be dropped for budget, and the target must not
        // vanish with its replacement. Optional blocks retire each other below,
        // once it is known which of them are sent. And derived text cannot
        // retire what the host must send - a note that names the user's
        // instruction in `supersedes` would otherwise delete it.
        let superseded_ids = mandatory
            .iter()
            .map(|(by, _)| by)
            .flat_map(|by| {
                mandatory
                    .iter()
                    .map(|(target, _)| target)
                    .chain(optional.iter())
                    .filter(move |target| {
                        target.id != by.id
                            && by.supersedes.contains(&target.id)
                            && may_supersede(by, target)
                    })
                    .map(|target| target.id.clone())
            })
            .collect::<std::collections::BTreeSet<_>>();
        let mut superseded_block_ids = Vec::new();
        let mut block_usage = Vec::new();
        mandatory.retain(|(block, _)| {
            let replaced = superseded_ids.contains(&block.id);
            if replaced {
                superseded_block_ids.push(block.id.clone());
                block_usage.push(usage(
                    block,
                    render_block(block).len(),
                    false,
                    Some(SUPERSEDED),
                ));
            }
            !replaced
        });
        let sent_earlier = |block: &ContextBlock| {
            block.kind != ContextBlockKind::Instruction && {
                let rendered = render_block(block);
                request
                    .sent_earlier
                    .iter()
                    .any(|earlier| earlier.contains(&rendered))
            }
        };
        mandatory.retain(|(block, _)| {
            let sent = sent_earlier(block);
            if sent {
                block_usage.push(usage(
                    block,
                    render_block(block).len(),
                    false,
                    Some(SENT_EARLIER),
                ));
            }
            !sent
        });
        let mandatory_rendered = mandatory
            .iter()
            .map(|(block, head)| (render_block(block), *head))
            .collect::<Vec<_>>();
        for ((block, _), (rendered, _)) in mandatory.iter().zip(&mandatory_rendered) {
            block_usage.push(usage(block, rendered.len(), true, None));
        }

        let mandatory_bytes = mandatory_rendered
            .iter()
            .map(|(rendered, _)| rendered.len())
            .sum::<usize>()
            .saturating_add(2 * mandatory_rendered.len().saturating_sub(1));
        let mandatory_tokens = estimate_bytes(fixed_bytes.saturating_add(mandatory_bytes));
        if mandatory_tokens > available {
            return Err(ContextError::new(
                harness_types::ErrorCode::MandatoryContextOverflow,
                format!(
                    "mandatory context requires {mandatory_tokens} tokens but only {available} are available"
                ),
            ));
        }

        // A derived block is optional whatever its producer claimed, and a block
        // that was superseded is not sent at all.
        for block in &mut optional {
            if !block.channel.may_be_mandatory() {
                block.mandatory = false;
            }
        }
        optional.retain(|block| {
            if sent_earlier(block) {
                block_usage.push(usage(
                    block,
                    render_block(block).len(),
                    false,
                    Some(SENT_EARLIER),
                ));
                return false;
            }
            if superseded_ids.contains(&block.id) {
                superseded_block_ids.push(block.id.clone());
                block_usage.push(usage(
                    block,
                    render_block(block).len(),
                    false,
                    Some(SUPERSEDED),
                ));
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
        let mut omitted_optional = Vec::new();
        let optional_budget = request
            .optional_token_budget
            .min(available - mandatory_tokens);
        let mut selected = Vec::<(ContextBlock, String)>::new();
        let mut selected_bytes = mandatory_bytes;
        for block in optional {
            let rendered = render_block(&block);
            let candidate_bytes = selected_bytes
                .saturating_add(2)
                .saturating_add(rendered.len());
            let candidate_optional_tokens =
                estimate_bytes(fixed_bytes.saturating_add(candidate_bytes))
                    .saturating_sub(mandatory_tokens);
            if candidate_optional_tokens <= optional_budget {
                selected_bytes = candidate_bytes;
                selected.push((block, rendered));
            } else {
                block_usage.push(usage(&block, rendered.len(), false, Some(OVER_BUDGET)));
                omitted_optional.push(block.id);
            }
        }
        // Now that it is known which optional blocks are sent, a sent one
        // retires the optional blocks it supersedes. Removing a block only
        // shrinks the request, so the budget still holds.
        let retired_by_optional = selected
            .iter()
            .flat_map(|(by, _)| {
                selected
                    .iter()
                    .filter(move |(target, _)| {
                        target.id != by.id
                            && by.supersedes.contains(&target.id)
                            && may_supersede(by, target)
                    })
                    .map(|(target, _)| target.id.clone())
            })
            .collect::<std::collections::BTreeSet<_>>();
        let mut optional_kept = Vec::new();
        let mut optional_rendered = Vec::new();
        for (block, rendered) in selected {
            if retired_by_optional.contains(&block.id) {
                superseded_block_ids.push(block.id.clone());
                block_usage.push(usage(&block, rendered.len(), false, Some(SUPERSEDED)));
                continue;
            }
            block_usage.push(usage(&block, rendered.len(), true, None));
            optional_rendered.push(rendered);
            optional_kept.push(block);
        }
        // The rules and facts, then the optional blocks by rank, then the
        // instruction, the working state and the tail.
        let content = mandatory_rendered
            .iter()
            .filter(|(_, head)| *head)
            .map(|(rendered, _)| rendered.as_str())
            .chain(optional_rendered.iter().map(String::as_str))
            .chain(
                mandatory_rendered
                    .iter()
                    .filter(|(_, head)| !*head)
                    .map(|(rendered, _)| rendered.as_str()),
            )
            .collect::<Vec<_>>()
            .join("\n\n");
        let optional_tokens = if optional_kept.is_empty() {
            0
        } else {
            estimate_bytes(fixed_bytes.saturating_add(content.len()))
                .saturating_sub(mandatory_tokens)
        };
        let channel_digest = ContentHash::from_canonical_json(&serde_json::json!({
            "mandatory": mandatory
                .iter()
                .map(|(block, _)| serde_json::json!({
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
        for entry in &mut block_usage {
            entry.cut_from_tokens = cuts.get(&entry.block_id).copied();
        }
        block_usage.sort_by(|left, right| left.block_id.cmp(&right.block_id));
        let degradation =
            (!omitted_optional.is_empty()).then(|| "optional_context_omitted".to_owned());
        let cut_block_ids = block_usage
            .iter()
            .filter(|entry| entry.included && entry.cut_from_tokens.is_some())
            .map(|entry| entry.block_id.clone())
            .collect();
        Ok(ContextBuildResult {
            packet,
            manifest,
            mandatory_block_ids: mandatory
                .iter()
                .map(|(block, _)| block.id.clone())
                .collect(),
            optional_block_ids: optional_kept.iter().map(|block| block.id.clone()).collect(),
            omitted_optional,
            cut_block_ids,
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
///
/// 3 renders the working state as facts the model can act on instead of the raw
/// projection JSON, whose refs it cannot resolve, and omits the block when there
/// is nothing in it yet.
pub(crate) const RENDERING_VERSION: u16 = 3;

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

/// The working state the model is shown, as facts instead of bookkeeping.
///
/// The durable projection is keyed by refs - `objective_ref`, instruction ids,
/// decision refs - because that is how the host proves where each field came
/// from. The model cannot resolve any of them: there is no tool that turns an
/// event id into text. Sending the raw JSON made a run open with a wall of
/// identifiers and a model that went looking for them, grepping the workspace
/// for `instruction_01a0...` instead of answering the question it was asked.
/// The refs stay in the packet's source manifest, where provenance belongs;
/// what the model reads is what it can act on, and nothing when there is
/// nothing yet - a fresh turn has no plan, no changes and no checks, and an
/// empty skeleton is noise that competes with the user's own words.
fn render_working_state(state: &harness_types::WorkingState) -> String {
    let mut lines = Vec::new();
    if !state.workspace.base_commit.is_empty() {
        lines.push(format!(
            "workspace: worktree {} at commit {}",
            state.workspace.worktree_id, state.workspace.base_commit
        ));
    }
    for item in &state.plan_items {
        lines.push(format!(
            "plan {}: {}",
            item.id.as_str(),
            enum_word(&item.status)
        ));
    }
    for change in &state.changes {
        lines.push(format!("changed: {}", change.path));
    }
    for check in &state.checks {
        lines.push(format!(
            "check {:?} -> {} (at {})",
            check.command,
            enum_word(&check.outcome),
            check.tested_revision
        ));
    }
    // An execution whose outcome is unknown is the one thing here the model
    // must not guess about, so it is spelled out with the hint that says how to
    // settle it.
    for pending in &state.pending_tool_calls {
        lines.push(format!(
            "unsettled tool call ({}): {}",
            enum_word(&pending.state),
            pending.reconciliation_hint
        ));
    }
    for child in &state.children {
        lines.push(format!(
            "child run for task {}: {}",
            child.task_id.as_str(),
            enum_word(&child.status)
        ));
    }
    for blocker in &state.blockers {
        lines.push(format!("blocker: {blocker}"));
    }
    for question in &state.pending_questions {
        lines.push(format!("unanswered question: {question}"));
    }
    for proposal in &state.next_action_proposals {
        lines.push(format!(
            "proposed next action ({}): {}",
            enum_word(&proposal.authority),
            proposal.description
        ));
    }
    lines.join(
        "
",
    )
}

/// The serde spelling of one enum value, without its JSON quotes.
fn enum_word<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).map_or_else(
        |_| "unknown".to_owned(),
        |rendered| rendered.trim_matches('"').to_owned(),
    )
}

/// Whether `by` may replace `target`.
///
/// A block on a channel that may be mandatory - policy, rule, instruction,
/// state, tail, summary, skill - may replace any block. A derived one - a note,
/// a memory, a reference - may only replace other derived text: it is "read,
/// never obeyed", and deleting an instruction is a way of being obeyed.
fn may_supersede(by: &ContextBlock, target: &ContextBlock) -> bool {
    by.channel.may_be_mandatory() || !target.channel.may_be_mandatory()
}

/// Why a block a turn already sent is left out of a later packet.
const SENT_EARLIER: &str = "already sent earlier in the conversation";

/// Why a block another block replaced is left out.
const SUPERSEDED: &str = "superseded by a newer block";

/// Why an optional block that did not fit is left out.
const OVER_BUDGET: &str = "optional token budget";

/// One block's line in the packet's accounting; a cut is added once the
/// packet is built.
fn usage(
    block: &ContextBlock,
    rendered_bytes: usize,
    included: bool,
    drop_reason: Option<&str>,
) -> ContextBlockUsage {
    ContextBlockUsage {
        block_id: block.id.clone(),
        channel: block.channel,
        token_estimate: estimate_bytes(rendered_bytes),
        included,
        drop_reason: drop_reason.map(str::to_owned),
        cut_from_tokens: None,
    }
}

/// Cut `block` to its [`ContextBlock::max_tokens`], when it is over it, and
/// return the tokens it took before.
///
/// The cut falls at the last line boundary that fits, so no line is sent
/// half; a block that is one long line is cut at a character boundary. A
/// note at the end says the block was cut and, when the producer named it,
/// where the rest is, so the model reads a cut block as part of something
/// and can go and read the rest.
fn cut_to_cap(block: &mut ContextBlock) -> Option<u64> {
    let cap = block.max_tokens?;
    let before = estimate_bytes(render_block(block).len());
    if before <= cap {
        return None;
    }
    let note = block.rest_at.as_deref().map_or_else(
        || format!("[cut to about {cap} of {before} tokens; the rest is left out]"),
        |rest| format!("[cut to about {cap} of {before} tokens; the rest is in {rest}]"),
    );
    // `[label]\n`, the text, then `\n` and the note.
    let frame = block.label().len() + 3 + 1 + note.len();
    let room = usize::try_from(cap)
        .unwrap_or(usize::MAX)
        .saturating_mul(4)
        .saturating_sub(frame)
        .min(block.text.len());
    let mut end = room;
    while !block.text.is_char_boundary(end) {
        end -= 1;
    }
    let kept = &block.text[..end];
    let kept = if block.text[end..].starts_with('\n') {
        kept
    } else {
        kept.rfind('\n').map_or(kept, |line_end| &kept[..line_end])
    };
    let text = if kept.trim().is_empty() {
        note
    } else {
        format!("{}\n{note}", kept.trim_end())
    };
    block.digest = ContentHash::from_bytes(text.as_bytes());
    block.text = text;
    Some(before)
}

fn render_block(block: &ContextBlock) -> String {
    format!("[{}]\n{}", block.label(), block.text)
}

fn estimate_bytes(bytes: usize) -> u64 {
    u64::try_from(bytes.saturating_add(3) / 4)
        .unwrap_or(u64::MAX)
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::{
        ContextBlock, ContextBlockKind, ContextBuildRequest, ContextBuilder, ContextChannel,
        ContextManifestInputs, RENDERING_VERSION, render_block, render_working_state,
    };

    fn working_state() -> harness_types::WorkingState {
        serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "session_id": "session_01a0d25b-22ca-762c-8f13-fffd33d30f10",
            "task_id": "task_01a0d25b-22ca-762c-8f13-fffe7b5b4468",
            "revision": 1,
            "through_event_seq": 1,
            "objective_ref": {
                "event_id": "event_01a0d25b-234f-7357-806a-ae68859b94e0",
                "sequence": 1,
                "content_hash": "sha256:3eb1bde86a1acbb65d48497f780b438baeea646acbe6bb360192bd74e6ea49f8"
            },
            "acceptance_criteria_refs": [],
            "active_instruction_refs": ["instruction_01a0d25b-234f-7357-806a-ae69db7c3b7c"],
            "decision_refs": [],
            "superseded_decision_refs": [],
            "plan_items": [],
            "workspace": {
                "project_id": "project_01a0d25b-1ebd-7274-be25-b24a75108689",
                "worktree_id": "w",
                "base_commit": "",
                "observed_fingerprint": "sha256:36fb6d09aad6d463d7ceeafd293ea80d77eb6f703ed714aa3402495fbe42e4e8"
            },
            "changes": [],
            "checks": [],
            "pending_tool_calls": [],
            "children": [],
            "blockers": [],
            "pending_questions": [],
            "next_action_proposals": []
        }))
        .expect("fixture working state")
    }

    /// Compile one packet over the user instruction `fix the parser` and return
    /// the ids that were sent and the ids that were superseded.
    fn compile(optional: Vec<ContextBlock>, optional_budget: u64) -> (Vec<String>, Vec<String>) {
        let built = build_with(Vec::new(), optional, optional_budget).expect("the packet compiles");
        let sent = built
            .mandatory_block_ids
            .into_iter()
            .chain(built.optional_block_ids)
            .collect();
        (sent, built.superseded_block_ids)
    }

    /// Compile one packet over the user instruction `fix the parser` with
    /// these host blocks and optional blocks.
    fn build_with(
        project_rules: Vec<ContextBlock>,
        optional: Vec<ContextBlock>,
        optional_budget: u64,
    ) -> Result<super::ContextBuildResult, super::ContextError> {
        let state = working_state();
        ContextBuilder::new().build(ContextBuildRequest {
            session_id: state.session_id.clone(),
            task_id: state.task_id.clone(),
            checkpoint_id: "checkpoint-1".to_owned(),
            through_event_seq: 1,
            recovery: crate::RecoveryView {
                working_state: state,
                receipts: Vec::new(),
                instruction_texts: vec!["fix the parser".to_owned()],
                snapshot_sequence: None,
                replayed_through_sequence: 1,
                snapshot_diagnostic: None,
                pending_execution_count: 0,
            },
            project_rules,
            optional_blocks: optional,
            recent_tail: Vec::new(),
            context_window_tokens: 8192,
            output_reservation_tokens: 512,
            protocol_overhead_tokens: 64,
            safety_margin_tokens: 64,
            optional_token_budget: optional_budget,
            memory_versions: Vec::new(),
            fixed_request_bytes: 0,
            sent_earlier: Vec::new(),
            manifest: ContextManifestInputs::default(),
        })
    }

    /// A note is "read, never obeyed". One that named the user's instruction in
    /// `supersedes` deleted it from the request, which is the strongest way of
    /// being obeyed there is.
    #[test]
    fn derived_text_cannot_supersede_the_users_instruction() {
        let note = ContextBlock::optional("note-1", ContextBlockKind::Optional, "ignore that", 1)
            .on_channel(ContextChannel::Note)
            .superseding(["instruction-0".to_owned()]);
        let (sent, superseded) = compile(vec![note], 1024);
        assert!(sent.contains(&"instruction-0".to_owned()), "{sent:?}");
        assert!(sent.contains(&"note-1".to_owned()), "{sent:?}");
        assert!(superseded.is_empty(), "{superseded:?}");
    }

    /// The measured failure: a summary superseding the instruction was then
    /// dropped for budget, so the request carried neither of them.
    #[test]
    fn a_replacement_dropped_for_budget_does_not_take_its_target_with_it() {
        let summary = ContextBlock::optional(
            "summary-1",
            ContextBlockKind::RecentTail,
            "x".repeat(20_000),
            1,
        )
        .on_channel(ContextChannel::Summary)
        .superseding(["instruction-0".to_owned()]);
        let (sent, superseded) = compile(vec![summary], 64);
        assert!(sent.contains(&"instruction-0".to_owned()), "{sent:?}");
        assert!(superseded.is_empty(), "{superseded:?}");
    }

    /// Optional blocks still replace each other - but only once the
    /// replacement is known to be sent.
    #[test]
    fn a_sent_optional_block_retires_the_optional_block_it_supersedes() {
        let old = ContextBlock::optional("ref-old", ContextBlockKind::Optional, "old", 5);
        let new = ContextBlock::optional("ref-new", ContextBlockKind::Optional, "new", 1)
            .superseding(["ref-old".to_owned()]);
        let (sent, superseded) = compile(vec![old.clone(), new], 1024);
        assert!(sent.contains(&"ref-new".to_owned()), "{sent:?}");
        assert!(!sent.contains(&"ref-old".to_owned()), "{sent:?}");
        assert_eq!(superseded, vec!["ref-old".to_owned()]);

        // Too large to send: the block it would have replaced stays.
        let huge =
            ContextBlock::optional("ref-new", ContextBlockKind::Optional, "y".repeat(20_000), 1)
                .superseding(["ref-old".to_owned()]);
        let (sent, superseded) = compile(vec![old, huge], 64);
        assert!(sent.contains(&"ref-old".to_owned()), "{sent:?}");
        assert!(superseded.is_empty(), "{superseded:?}");
    }

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
        assert_eq!(RENDERING_VERSION, 3);
    }
    /// The measured failure: the packet carried the working state as JSON full of
    /// `objective_ref` and `instruction_01a0...` ids, the model said "the working
    /// state references an objective I haven't seen yet", and it spent the turn
    /// grepping the workspace for an event id instead of answering.
    #[test]
    fn the_working_state_is_rendered_without_refs_the_model_cannot_resolve() {
        let state: harness_types::WorkingState = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "session_id": "session_01a0d25b-22ca-762c-8f13-fffd33d30f10",
            "task_id": "task_01a0d25b-22ca-762c-8f13-fffe7b5b4468",
            "revision": 7,
            "through_event_seq": 7,
            "objective_ref": {
                "event_id": "event_01a0d25b-234f-7357-806a-ae68859b94e0",
                "sequence": 1,
                "content_hash": "sha256:3eb1bde86a1acbb65d48497f780b438baeea646acbe6bb360192bd74e6ea49f8"
            },
            "acceptance_criteria_refs": [],
            "active_instruction_refs": ["instruction_01a0d25b-234f-7357-806a-ae69db7c3b7c"],
            "decision_refs": [],
            "superseded_decision_refs": [],
            "plan_items": [],
            "workspace": {
                "project_id": "project_01a0d25b-1ebd-7274-be25-b24a75108689",
                "worktree_id": "p3-fa3f15bbec1b0f58",
                "base_commit": "cf66c8343883f2791df2733097f4027e61f82558",
                "observed_fingerprint": "sha256:36fb6d09aad6d463d7ceeafd293ea80d77eb6f703ed714aa3402495fbe42e4e8"
            },
            "changes": [{"path": "src/parser.rs", "before_hash": null, "after_hash": null, "tool_execution_id": null}],
            "checks": [],
            "pending_tool_calls": [],
            "children": [],
            "blockers": ["waiting for the fixture"],
            "pending_questions": [],
            "next_action_proposals": []
        }))
        .expect("fixture working state");
        let rendered = render_working_state(&state);
        for opaque in [
            "objective_ref",
            "instruction_01a0",
            "event_01a0",
            "sha256:",
            "session_01a0",
        ] {
            assert!(
                !rendered.contains(opaque),
                "{opaque} is bookkeeping the model cannot resolve: {rendered}"
            );
        }
        assert!(rendered.contains("changed: src/parser.rs"), "{rendered}");
        assert!(
            rendered.contains("blocker: waiting for the fixture"),
            "{rendered}"
        );
        assert!(rendered.contains("cf66c8343883"), "{rendered}");
    }

    /// The measured failure: every host block reached the builder as a
    /// project rule and was sent whatever its flag, so a new conversation
    /// carried ~60K tokens of blocks the optional budget never saw. A host
    /// block marked optional is now ranked by relevance and admitted within
    /// the budget; the one that does not fit is reported as dropped.
    #[test]
    fn a_host_block_marked_optional_is_ranked_into_the_optional_budget() {
        let rule = ContextBlock::mandatory("rule", ContextBlockKind::ProjectRule, "keep it small");
        let high =
            ContextBlock::optional("digest", ContextBlockKind::Optional, "a".repeat(400), 70);
        let low = ContextBlock::optional("stale", ContextBlockKind::Optional, "b".repeat(400), 10);
        let built = build_with(vec![rule, low, high], Vec::new(), 150).expect("compiles");
        assert!(built.mandatory_block_ids.contains(&"rule".to_owned()));
        assert_eq!(built.optional_block_ids, vec!["digest".to_owned()]);
        assert_eq!(built.omitted_optional, vec!["stale".to_owned()]);
        let dropped = built
            .block_usage
            .iter()
            .find(|usage| usage.block_id == "stale")
            .expect("the dropped block is reported");
        assert!(!dropped.included);
        assert_eq!(
            dropped.drop_reason.as_deref(),
            Some("optional token budget")
        );

        // With room for both, both are sent, the higher ranked first.
        let built = build_with(
            vec![
                ContextBlock::optional("stale", ContextBlockKind::Optional, "low one", 10),
                ContextBlock::optional("digest", ContextBlockKind::Optional, "high one", 70),
            ],
            Vec::new(),
            4096,
        )
        .expect("compiles");
        assert_eq!(
            built.optional_block_ids,
            vec!["digest".to_owned(), "stale".to_owned()]
        );
        let content = &built.packet.content;
        assert!(
            content.find("high one") < content.find("low one"),
            "{content}"
        );
    }

    /// The optional blocks are sent after the rules and before the user's
    /// instruction, so the request still ends with what the turn is about.
    #[test]
    fn the_users_instruction_comes_after_the_optional_blocks() {
        let rule = ContextBlock::mandatory("rule", ContextBlockKind::ProjectRule, "the rule");
        let optional =
            ContextBlock::optional("reference", ContextBlockKind::Optional, "the reference", 40);
        let built = build_with(vec![rule, optional], Vec::new(), 4096).expect("compiles");
        let content = &built.packet.content;
        let rule_at = content.find("the rule").expect("rule");
        let reference_at = content.find("the reference").expect("reference");
        let instruction_at = content.find("fix the parser").expect("instruction");
        assert!(
            rule_at < reference_at && reference_at < instruction_at,
            "{content}"
        );
        assert_eq!(
            built.packet.token_estimate,
            super::estimate_bytes(content.len())
        );
    }

    /// A block over its cap is cut at a line boundary, before admission,
    /// with a note naming where the rest is, and the cut is reported.
    #[test]
    fn a_block_over_its_cap_is_cut_at_a_line_with_a_note() {
        let text = (0..200)
            .map(|line| format!("line {line} of the session log"))
            .collect::<Vec<_>>()
            .join("\n");
        let block = ContextBlock::optional("session-log", ContextBlockKind::Optional, text, 30)
            .with_max_tokens(100)
            .with_rest_at(".harness/sessions/log.md");
        let built = build_with(vec![block], Vec::new(), 4096).expect("compiles");
        assert_eq!(built.optional_block_ids, vec!["session-log".to_owned()]);
        assert_eq!(built.cut_block_ids, vec!["session-log".to_owned()]);
        let usage = built
            .block_usage
            .iter()
            .find(|usage| usage.block_id == "session-log")
            .expect("usage");
        assert!(usage.included);
        assert!(usage.token_estimate <= 100, "{usage:?}");
        assert!(
            usage.cut_from_tokens.is_some_and(|before| before > 100),
            "{usage:?}"
        );
        let content = &built.packet.content;
        let start = content.find("[reference session-log]").expect("block");
        let rendered = &content[start
            ..content[start..]
                .find("\n\n")
                .map_or(content.len(), |end| start + end)];
        assert!(
            rendered.contains("the rest is in .harness/sessions/log.md]"),
            "{rendered}"
        );
        // Every line kept is whole.
        for line in rendered.lines().skip(1) {
            assert!(
                line.ends_with("of the session log") || line.starts_with("[cut to about 100 of "),
                "a line was cut in half: {line:?}"
            );
        }
        assert!(!rendered.contains("line 199 "), "{rendered}");
    }

    /// A mandatory block over its cap is sent cut instead of failing the
    /// whole request with an overflow.
    #[test]
    fn a_mandatory_block_over_its_cap_is_cut_instead_of_overflowing() {
        let huge = "a line of the task state\n".repeat(2_000);
        let uncapped = ContextBlock::mandatory("task-state", ContextBlockKind::Instruction, &*huge);
        let error = build_with(vec![uncapped], Vec::new(), 0).expect_err("overflows");
        assert_eq!(
            error.code(),
            harness_types::ErrorCode::MandatoryContextOverflow
        );
        let capped = ContextBlock::mandatory("task-state", ContextBlockKind::Instruction, huge)
            .with_max_tokens(1_000);
        let built = build_with(vec![capped], Vec::new(), 0).expect("cut, not overflowed");
        assert!(built.mandatory_block_ids.contains(&"task-state".to_owned()));
        assert_eq!(built.cut_block_ids, vec!["task-state".to_owned()]);
        assert!(
            built.mandatory_tokens <= 1_100,
            "{}",
            built.mandatory_tokens
        );
        assert!(
            built.packet.content.contains("; the rest is left out]"),
            "a cut with nowhere named says the rest is left out"
        );
    }

    /// A block within its cap is sent as it is.
    #[test]
    fn a_block_within_its_cap_is_not_cut() {
        let block = ContextBlock::optional("small", ContextBlockKind::Optional, "short\ntext", 1)
            .with_max_tokens(100);
        let built = build_with(vec![block], Vec::new(), 4096).expect("compiles");
        assert!(built.cut_block_ids.is_empty());
        assert!(
            built
                .packet
                .content
                .contains("[reference small]\nshort\ntext")
        );
    }
}
