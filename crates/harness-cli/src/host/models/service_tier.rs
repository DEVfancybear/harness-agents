//! prime-agent's service tiers (`/tier`, `/fast`): which tiers a model takes and
//! what the status line calls the one in force (`packages/ai/src/models.ts`,
//! `interactive-mode.ts`).

/// The tiers `/tier` offers, in prime-agent's order.
pub const CHOICES: [&str; 4] = ["default", "flex", "priority", "auto"];

/// prime-agent's description of each tier, for the argument menu.
#[must_use]
pub fn description(tier: &str) -> &'static str {
    match tier {
        "default" => "Standard processing",
        "flex" => "Cheaper, slower, may hit capacity limits",
        "priority" => "Faster, more expensive (fast mode)",
        "auto" => "Provider picks the tier",
        _ => "",
    }
}

/// prime-agent's `supportsServiceTier`. The `OpenAI` Responses API and the
/// `ChatGPT` (Codex) backend take a tier: `priority` (`/fast`) for the models
/// `OpenAI` lists for it, `flex` for the GPT-5 ones of those on the API only,
/// and `auto` on the API only. Every other provider takes the default only.
/// (prime-agent also sends `OpenRouter` tiers, a provider ha does not have.)
///
/// Seen against the `ChatGPT` backend (2026-11-07): it answers `priority` for
/// `gpt-6.1-sol`, `gpt-6.1-terra` and every `gpt-6-*` model as it does for
/// `gpt-5.6`, and refuses `auto` and `flex` with "Unsupported `service_tier`"
/// for every model - so 0.9.2's `/fast` said `gpt-6.1-sol` had no fast mode,
/// and its `/tier auto` failed every turn on the backend.
#[must_use]
pub fn supports(provider_id: &str, protocol: &str, model: &str, tier: &str) -> bool {
    if tier == "default" {
        return true;
    }
    let responses = provider_id == "openai" && protocol == "openai_responses";
    let codex = provider_id == "openai-codex" && protocol == "openai_codex";
    if !responses && !codex {
        return false;
    }
    let gpt5 = matches!(model, "gpt-5.4" | "gpt-5.5" | "gpt-5.6") || model.starts_with("gpt-5.6-");
    match tier {
        "auto" => responses,
        "priority" => gpt5 || gpt6(model),
        "flex" => (gpt5 || model == "gpt-6-astra") && responses,
        _ => false,
    }
}

/// The GPT-6 family: `gpt-6-<name>` and a point release, `gpt-6.<n>-<name>`.
fn gpt6(model: &str) -> bool {
    let Some(rest) = model.strip_prefix("gpt-6") else {
        return false;
    };
    let rest = match rest.strip_prefix('.') {
        Some(point) => {
            let name = point.trim_start_matches(|c: char| c.is_ascii_digit());
            if name.len() == point.len() {
                return false;
            }
            name
        }
        None => rest,
    };
    rest.strip_prefix('-').is_some_and(|name| !name.is_empty())
}

/// The tiers this model takes, in [`CHOICES`] order.
#[must_use]
pub fn available(provider_id: &str, protocol: &str, model: &str) -> Vec<&'static str> {
    CHOICES
        .into_iter()
        .filter(|tier| supports(provider_id, protocol, model, tier))
        .collect()
}

/// prime-agent's `clampServiceTier`: the requested tier when the model takes it,
/// else `default`. `None` - nothing requested - stays `None`, and nothing is sent.
#[must_use]
pub fn clamp(
    requested: Option<&str>,
    provider_id: &str,
    protocol: &str,
    model: &str,
) -> Option<String> {
    requested.map(|tier| {
        if supports(provider_id, protocol, model, tier) {
            tier.to_owned()
        } else {
            "default".to_owned()
        }
    })
}

/// What the status line adds after the model: `fast` for priority, the tier's
/// name for another non-default one, nothing otherwise.
#[must_use]
pub fn status_label(tier: Option<&str>) -> Option<String> {
    match tier {
        None | Some("default") => None,
        Some("priority") => Some("fast".to_owned()),
        Some(other) => Some(other.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::{available, clamp, status_label, supports};

    #[test]
    fn prime_agents_support_rules() {
        assert_eq!(
            available("openai", "openai_responses", "gpt-5.5"),
            ["default", "flex", "priority", "auto"]
        );
        assert_eq!(
            available("openai-codex", "openai_codex", "gpt-5.6-luna"),
            ["default", "priority"],
            "the ChatGPT backend refuses flex and auto"
        );
        assert_eq!(
            available("openai", "openai_responses", "gpt-4.1"),
            ["default", "auto"]
        );
        assert_eq!(
            available("deepseek", "openai_chat", "deepseek-v4"),
            ["default"]
        );
        assert!(!supports("openai", "openai_chat", "gpt-5.5", "priority"));
    }

    /// 0.9.2's `/fast` on `openai-codex/gpt-6.1-sol` said the model had no fast
    /// mode, while the backend serves it on `priority`.
    #[test]
    fn the_gpt_6_family_has_fast_mode() {
        for model in [
            "gpt-6.1-sol",
            "gpt-6.1-terra",
            "gpt-6-sol",
            "gpt-6-luna",
            "gpt-6-astra",
            "gpt-6.12-nova",
        ] {
            assert_eq!(
                available("openai-codex", "openai_codex", model),
                ["default", "priority"],
                "{model}"
            );
            assert!(
                supports("openai", "openai_responses", model, "priority"),
                "{model}"
            );
        }
        for model in ["gpt-6", "gpt-6.-sol", "gpt-6.1", "gpt-60-sol", "gpt-6x-sol"] {
            assert!(
                !supports("openai-codex", "openai_codex", model, "priority"),
                "{model}"
            );
        }
        assert!(!supports(
            "openai",
            "openai_responses",
            "gpt-6.1-sol",
            "flex"
        ));
        assert_eq!(
            clamp(Some("auto"), "openai-codex", "openai_codex", "gpt-6.1-sol").as_deref(),
            Some("default"),
            "the ChatGPT backend answers auto with a 400"
        );
    }

    #[test]
    fn a_tier_the_model_lacks_is_clamped_to_default() {
        assert_eq!(clamp(None, "openai", "openai_responses", "gpt-5.5"), None);
        assert_eq!(
            clamp(Some("priority"), "deepseek", "openai_chat", "deepseek-v4").as_deref(),
            Some("default")
        );
        assert_eq!(
            clamp(Some("priority"), "openai-codex", "openai_codex", "gpt-5.6").as_deref(),
            Some("priority")
        );
    }

    #[test]
    fn the_status_line_calls_priority_fast() {
        assert_eq!(status_label(Some("priority")).as_deref(), Some("fast"));
        assert_eq!(status_label(Some("flex")).as_deref(), Some("flex"));
        assert_eq!(status_label(Some("default")), None);
        assert_eq!(status_label(None), None);
    }
}
