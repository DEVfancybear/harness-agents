//! Thinking levels, ported from prime-agent's `pi-ai` (`models.ts`,
//! `providers/openai-completions.ts`, `providers/anthropic.ts`,
//! `providers/simple-options.ts`).
//!
//! A session picks one of seven levels. A model supports a subset of them, described
//! by its level map the way `pi-ai`'s generated model table does: a level mapped to
//! `None` is not offered, `xhigh` and `max` are offered only when mapped, and a level
//! a model lacks is clamped to the nearest one it has - upwards first, as
//! `clampThinkingLevel` does. A model no table knows is taken as not reasoning, as
//! `pi-ai` takes a custom model, unless the configuration says it reasons.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// A reasoning effort, from none to the most a model offers.
#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    #[default]
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ThinkingLevel {
    /// Every level, from `off` to `max` (`pi-ai`'s `EXTENDED_THINKING_LEVELS`).
    pub const ALL: [Self; 7] = [
        Self::Off,
        Self::Minimal,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::Xhigh,
        Self::Max,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// The level a name spells, case-insensitively.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|level| level.as_str() == name)
    }

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|level| *level == self)
            .unwrap_or_default()
    }
}

impl std::fmt::Display for ThinkingLevel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// How a model's endpoint takes a thinking level (`pi-ai`'s `thinkingFormat`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThinkingFormat {
    /// `DeepSeek`: `thinking: {type}` plus `reasoning_effort`, and every assistant
    /// message carries `reasoning_content` back.
    DeepSeek,
    /// `OpenAI`-compatible `reasoning_effort`.
    ReasoningEffort,
    /// Anthropic Messages: adaptive thinking with an effort, or a token budget.
    Anthropic,
}

/// What `pi-ai` knows about one model's reasoning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReasoningModel {
    /// Whether the model reasons at all.
    pub reasoning: bool,
    /// The level map; `None` for a level means the model does not offer it, a
    /// missing level means the default spelling.
    pub map: &'static [(ThinkingLevel, Option<&'static str>)],
}

const DEEPSEEK_V4: &[(ThinkingLevel, Option<&str>)] = &[
    (ThinkingLevel::Minimal, None),
    (ThinkingLevel::Low, None),
    (ThinkingLevel::Medium, None),
    (ThinkingLevel::High, Some("high")),
    (ThinkingLevel::Xhigh, Some("max")),
    (ThinkingLevel::Max, None),
];

const CLAUDE_ADAPTIVE: &[(ThinkingLevel, Option<&str>)] = &[
    (ThinkingLevel::Xhigh, Some("xhigh")),
    (ThinkingLevel::Max, Some("max")),
];

/// The reasoning description of a model, from `pi-ai`'s generated table.
#[must_use]
pub fn reasoning_model(provider_id: &str, model: &str) -> Option<ReasoningModel> {
    let model = model.to_ascii_lowercase();
    if provider_id == "deepseek" || model.starts_with("deepseek") {
        // `deepseek-v4-flash` and `deepseek-v4-pro` (and the `deepseek-flash` alias
        // this app's preset uses) share `pi-ai`'s DeepSeek V4 map.
        if model.contains("v4") || model.contains("flash") || model.contains("pro") {
            return Some(ReasoningModel {
                reasoning: true,
                map: DEEPSEEK_V4,
            });
        }
        if model.contains("reasoner") {
            return Some(ReasoningModel {
                reasoning: true,
                map: &[],
            });
        }
        return None;
    }
    if model.contains("claude") {
        if supports_adaptive_thinking(&model) {
            return Some(ReasoningModel {
                reasoning: true,
                map: CLAUDE_ADAPTIVE,
            });
        }
        // Budget-based thinking on the Claude 3.7 / 4 generation.
        if model.contains("3-7") || model.contains("-4") {
            return Some(ReasoningModel {
                reasoning: true,
                map: &[],
            });
        }
    }
    None
}

/// The levels a model offers (`pi-ai`'s `getSupportedThinkingLevels`).
#[must_use]
pub fn supported_levels(model: Option<ReasoningModel>) -> Vec<ThinkingLevel> {
    let Some(model) = model.filter(|model| model.reasoning) else {
        return vec![ThinkingLevel::Off];
    };
    ThinkingLevel::ALL
        .into_iter()
        .filter(|level| {
            let mapped = model.map.iter().find(|(candidate, _)| candidate == level);
            match mapped {
                Some((_, None)) => false,
                Some((_, Some(_))) => true,
                None => !matches!(level, ThinkingLevel::Xhigh | ThinkingLevel::Max),
            }
        })
        .collect()
}

/// The level a model uses for `level` (`pi-ai`'s `clampThinkingLevel`).
#[must_use]
pub fn clamp(model: Option<ReasoningModel>, level: ThinkingLevel) -> ThinkingLevel {
    let available = supported_levels(model);
    if available.contains(&level) {
        return level;
    }
    let requested = level.index();
    ThinkingLevel::ALL[requested..]
        .iter()
        .chain(ThinkingLevel::ALL[..requested].iter().rev())
        .copied()
        .find(|candidate| available.contains(candidate))
        .unwrap_or(ThinkingLevel::Off)
}

/// The provider's spelling of a level, when the map renames it.
fn mapped(model: Option<ReasoningModel>, level: ThinkingLevel) -> String {
    model
        .and_then(|model| {
            model
                .map
                .iter()
                .find(|(candidate, _)| *candidate == level)
                .and_then(|(_, value)| *value)
        })
        .unwrap_or(level.as_str())
        .to_owned()
}

/// Adaptive-thinking Claude models (`pi-ai`'s `supportsAdaptiveThinking`).
#[must_use]
pub fn supports_adaptive_thinking(model: &str) -> bool {
    [
        "opus-4-6",
        "opus-4.6",
        "opus-5",
        "opus-4-7",
        "opus-4.7",
        "opus-4-8",
        "opus-4.8",
        "sonnet-4-6",
        "sonnet-4.6",
        "sonnet-5",
        "fable-5",
        "mythos-5",
        "mythos-preview",
    ]
    .iter()
    .any(|id| model.contains(id))
}

/// The resolved thinking of one adapter: the model's level, and how to send it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Thinking {
    pub level: ThinkingLevel,
    pub format: ThinkingFormat,
}

impl Thinking {
    /// Whether assistant messages must carry their reasoning back.
    #[must_use]
    pub fn replays_reasoning_content(&self) -> bool {
        self.format == ThinkingFormat::DeepSeek
    }

    /// Fill the request body's reasoning fields for `model` (`pi-ai`'s
    /// `buildParams` for Chat Completions).
    pub fn apply_chat(&self, body: &mut Value, provider_id: &str, model: &str) {
        let description = reasoning_model(provider_id, model);
        let level = clamp(description, self.level);
        let reasons = description.is_some_and(|model| model.reasoning);
        match self.format {
            ThinkingFormat::DeepSeek if reasons => {
                let enabled = level != ThinkingLevel::Off;
                body["thinking"] = json!({ "type": if enabled { "enabled" } else { "disabled" } });
                if enabled {
                    body["reasoning_effort"] = json!(mapped(description, level));
                }
            }
            // A DeepSeek model with no known map still has thinking on by default,
            // which fails the second request; turn it off unless a level was asked.
            ThinkingFormat::DeepSeek if self.level == ThinkingLevel::Off => {
                body["thinking"] = json!({ "type": "disabled" });
            }
            ThinkingFormat::ReasoningEffort if reasons && level != ThinkingLevel::Off => {
                body["reasoning_effort"] = json!(mapped(description, level));
            }
            _ => {}
        }
    }

    /// Fill an Anthropic Messages body (`pi-ai`'s `streamSimpleAnthropic`): adaptive
    /// thinking with an effort on the models that have it, a token budget - raising
    /// `max_tokens` to make room - on the others, and nothing when thinking is off.
    pub fn apply_anthropic(&self, body: &mut Value, model: &str) {
        let description = reasoning_model("anthropic", model);
        let level = clamp(description, self.level);
        if !description.is_some_and(|model| model.reasoning) {
            return;
        }
        if level == ThinkingLevel::Off {
            body["thinking"] = json!({ "type": "disabled" });
            return;
        }
        // Temperature is incompatible with extended thinking.
        if let Some(object) = body.as_object_mut() {
            object.remove("temperature");
        }
        if supports_adaptive_thinking(&model.to_ascii_lowercase()) {
            let effort = match level {
                ThinkingLevel::Xhigh | ThinkingLevel::Max => mapped(description, level),
                ThinkingLevel::Minimal | ThinkingLevel::Low => "low".to_owned(),
                ThinkingLevel::Medium => "medium".to_owned(),
                ThinkingLevel::High | ThinkingLevel::Off => "high".to_owned(),
            };
            body["thinking"] = json!({ "type": "adaptive", "display": "summarized" });
            body["output_config"] = json!({ "effort": effort });
            return;
        }
        // `adjustMaxTokensForThinking`: xhigh and max are clamped to high.
        let budget: u64 = match level {
            ThinkingLevel::Minimal | ThinkingLevel::Off => 1024,
            ThinkingLevel::Low => 2048,
            ThinkingLevel::Medium => 8192,
            ThinkingLevel::High | ThinkingLevel::Xhigh | ThinkingLevel::Max => 16_384,
        };
        let base = body["max_tokens"].as_u64().unwrap_or(4096);
        body["max_tokens"] = json!(base + budget);
        body["thinking"] = json!({
            "type": "enabled",
            "budget_tokens": budget,
            "display": "summarized",
        });
    }
}

/// The format of an endpoint that speaks `OpenAI` Chat Completions.
#[must_use]
pub fn chat_format(provider_id: &str, endpoint: &str) -> ThinkingFormat {
    if provider_id == "deepseek" || endpoint.contains("deepseek.com") {
        ThinkingFormat::DeepSeek
    } else {
        ThinkingFormat::ReasoningEffort
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Thinking, ThinkingFormat, ThinkingLevel, clamp, reasoning_model, supported_levels,
    };
    use serde_json::json;

    #[test]
    fn deepseek_v4_offers_off_high_and_xhigh_as_pi_ai_maps_it() {
        let model = reasoning_model("deepseek", "deepseek-v4-flash");
        assert_eq!(
            supported_levels(model),
            [
                ThinkingLevel::Off,
                ThinkingLevel::High,
                ThinkingLevel::Xhigh
            ]
        );
        // Upwards first, then downwards.
        assert_eq!(clamp(model, ThinkingLevel::Low), ThinkingLevel::High);
        assert_eq!(clamp(model, ThinkingLevel::Max), ThinkingLevel::Xhigh);
        let mut body = json!({});
        Thinking {
            level: ThinkingLevel::Xhigh,
            format: ThinkingFormat::DeepSeek,
        }
        .apply_chat(&mut body, "deepseek", "deepseek-v4-flash");
        assert_eq!(
            body,
            json!({"thinking": {"type": "enabled"}, "reasoning_effort": "max"})
        );
        let mut body = json!({});
        Thinking {
            level: ThinkingLevel::Off,
            format: ThinkingFormat::DeepSeek,
        }
        .apply_chat(&mut body, "deepseek", "deepseek-v4-flash");
        assert_eq!(body, json!({"thinking": {"type": "disabled"}}));
    }

    #[test]
    fn an_unknown_model_does_not_reason_like_a_pi_ai_custom_model() {
        assert_eq!(supported_levels(None), [ThinkingLevel::Off]);
        assert_eq!(clamp(None, ThinkingLevel::High), ThinkingLevel::Off);
        let mut body = json!({});
        Thinking {
            level: ThinkingLevel::High,
            format: ThinkingFormat::ReasoningEffort,
        }
        .apply_chat(&mut body, "openai", "local-model");
        assert_eq!(body, json!({}));
    }

    #[test]
    fn claude_uses_adaptive_effort_or_a_budget() {
        let mut body = json!({"max_tokens": 4096, "temperature": 0.2});
        Thinking {
            level: ThinkingLevel::Medium,
            format: ThinkingFormat::Anthropic,
        }
        .apply_anthropic(&mut body, "claude-sonnet-4-5");
        assert_eq!(body["thinking"]["budget_tokens"], 8192);
        assert_eq!(body["max_tokens"], 4096 + 8192);
        assert!(body.get("temperature").is_none());
        let mut body = json!({"max_tokens": 4096});
        Thinking {
            level: ThinkingLevel::Max,
            format: ThinkingFormat::Anthropic,
        }
        .apply_anthropic(&mut body, "claude-opus-5-5");
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"]["effort"], "max");
    }

    #[test]
    fn levels_parse_and_print() {
        for level in ThinkingLevel::ALL {
            assert_eq!(ThinkingLevel::parse(level.as_str()), Some(level));
        }
        assert_eq!(ThinkingLevel::parse(" HIGH "), Some(ThinkingLevel::High));
        assert_eq!(ThinkingLevel::parse("huge"), None);
    }
}
