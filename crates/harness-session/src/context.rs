use harness_types::{
    ContentHash, ContextPacket, ContextPacketId, MemoryAssetId, MemoryVersionRef,
    P0_SCHEMA_VERSION, SessionId, TaskId,
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
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextBlock {
    pub id: String,
    pub kind: ContextBlockKind,
    pub text: String,
    pub mandatory: bool,
    pub relevance: i32,
}

impl ContextBlock {
    #[must_use]
    pub fn mandatory(
        id: impl Into<String>,
        kind: ContextBlockKind,
        text: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            text: text.into(),
            mandatory: true,
            relevance: 0,
        }
    }
    #[must_use]
    pub fn optional(
        id: impl Into<String>,
        kind: ContextBlockKind,
        text: impl Into<String>,
        relevance: i32,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            text: text.into(),
            mandatory: false,
            relevance,
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
}

#[derive(Clone, Debug)]
pub struct ContextBuildResult {
    pub packet: ContextPacket,
    pub mandatory_block_ids: Vec<String>,
    pub optional_block_ids: Vec<String>,
    pub omitted_optional: Vec<String>,
    pub mandatory_tokens: u64,
    pub optional_tokens: u64,
    pub degradation: Option<String>,
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

impl ContextBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    #[allow(clippy::too_many_lines)]
    pub fn build(&self, request: ContextBuildRequest) -> Result<ContextBuildResult, ContextError> {
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
            block.mandatory = true;
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
            block.mandatory = true;
            mandatory.push(block);
        }
        let mut content = mandatory
            .iter()
            .map(render_block)
            .collect::<Vec<_>>()
            .join("\n\n");
        let mandatory_tokens = estimate_tokens(&content);
        if mandatory_tokens > available {
            return Err(ContextError::new(
                harness_types::ErrorCode::MandatoryContextOverflow,
                format!(
                    "mandatory context requires {mandatory_tokens} tokens but only {available} are available"
                ),
            ));
        }

        let mut optional = request.optional_blocks;
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
                estimate_bytes(candidate_bytes).saturating_sub(mandatory_tokens);
            if candidate_optional_tokens <= optional_budget {
                optional_tokens = candidate_optional_tokens;
                content.push_str("\n\n");
                content.push_str(&rendered);
                optional_kept.push(block);
            } else {
                omitted_optional.push(block.id);
            }
        }
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
            token_estimate: estimate_tokens(&content),
            content_hash: ContentHash::from_bytes(content.as_bytes()),
            content,
            source_manifest,
        };
        packet.validate().map_err(|error| {
            ContextError::new(error.code(), format!("context packet is invalid: {error}"))
        })?;
        let degradation =
            (!omitted_optional.is_empty()).then(|| "optional_context_omitted".to_owned());
        Ok(ContextBuildResult {
            packet,
            mandatory_block_ids: mandatory.iter().map(|block| block.id.clone()).collect(),
            optional_block_ids: optional_kept.iter().map(|block| block.id.clone()).collect(),
            omitted_optional,
            mandatory_tokens,
            optional_tokens,
            degradation,
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
        match self.kind {
            ContextBlockKind::SystemPolicy => "system policy".to_owned(),
            ContextBlockKind::Instruction => "user instruction".to_owned(),
            ContextBlockKind::WorkingState => "working state".to_owned(),
            ContextBlockKind::RecentTail => "earlier context".to_owned(),
            ContextBlockKind::ProjectRule => format!("project rule {}", self.id),
            ContextBlockKind::Memory => format!("memory {}", self.id),
            ContextBlockKind::Optional => format!("reference {}", self.id),
        }
    }
}

fn estimate_tokens(text: &str) -> u64 {
    estimate_bytes(text.len())
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
