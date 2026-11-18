//! Session cost calculations from configured per-million token prices.

#[derive(Clone, Copy, Debug, PartialEq)]
#[allow(clippy::struct_field_names)] // the catalog's price names
pub struct ModelPrice {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    /// prime-agent prices cached input apart: a read from the cache and a
    /// write to it. `None` prices them as ordinary input.
    pub cache_read_per_mtok: Option<f64>,
    pub cache_write_per_mtok: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[allow(clippy::struct_field_names)] // the provider's usage names
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Of `input_tokens`, read from and written to the prompt cache.
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

impl ModelPrice {
    /// The price a catalog entry lists, when it lists one.
    #[must_use]
    pub fn of_catalog(cost: &super::providers::Cost) -> Option<Self> {
        (cost.input > 0.0 || cost.output > 0.0).then_some(Self {
            input_per_mtok: cost.input,
            output_per_mtok: cost.output,
            cache_read_per_mtok: Some(cost.cache_read),
            cache_write_per_mtok: Some(cost.cache_write),
        })
    }
}

/// What model calls cost in US dollars, by the four kinds of token a price
/// has (arXiv 2611.12122: `C = pr*R + pw*W + pu*U + po*O`). `total` is the
/// whole, as `calculate_cost` returns it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CostParts {
    /// Prompt tokens that were neither read from the cache nor written to it.
    pub uncached_input: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub output: f64,
    pub total: f64,
}

impl CostParts {
    /// These parts and `other`'s together.
    #[must_use]
    pub fn plus(self, other: Self) -> Self {
        Self {
            uncached_input: self.uncached_input + other.uncached_input,
            cache_read: self.cache_read + other.cache_read,
            cache_write: self.cache_write + other.cache_write,
            output: self.output + other.output,
            total: self.total + other.total,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CostTracker {
    total_usd: f64,
    /// `total_usd` by the kind of token; they stop with it when a call is unpriced.
    parts: CostParts,
    has_usage: bool,
    unpriced_usage: bool,
    input_tokens: u64,
    output_tokens: u64,
    /// Of `input_tokens`, how many were read from and written to the prompt cache.
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    /// The last request's prompt plus its answer: what the context holds now, as
    /// prime-agent reads it off the latest response's usage.
    context_tokens: Option<u64>,
    /// The provider's measured prompt over the runtime's estimate of it, at
    /// the conversation's last response: what the next turn's compaction
    /// decision is scaled by.
    estimate_scale: Option<f64>,
}

impl CostTracker {
    pub fn record(&mut self, price: Option<ModelPrice>, usage: Usage) {
        self.has_usage = true;
        self.input_tokens = self.input_tokens.saturating_add(usage.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(usage.output_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(usage.cache_read_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(usage.cache_write_tokens);
        self.context_tokens = Some(usage.input_tokens.saturating_add(usage.output_tokens));
        match split_cost(price, usage) {
            Some(parts) if !self.unpriced_usage => {
                self.total_usd += parts.total;
                self.parts = self.parts.plus(parts);
            }
            Some(_) => {}
            None => self.unpriced_usage = true,
        }
    }

    /// The session's cost so far by the kind of token; `None` before any
    /// usage and when some of it had no price.
    #[must_use]
    pub const fn parts(&self) -> Option<CostParts> {
        if self.has_usage && !self.unpriced_usage {
            Some(self.parts)
        } else {
            None
        }
    }

    /// `/cost`'s second line: each kind of token's dollars, its share of the
    /// cost and its tokens, e.g. `cache read $0.0123 (61%, 1.2M tok) · ...`.
    /// `None` when the cost is not priced or is nothing.
    #[must_use]
    pub fn breakdown(&self) -> Option<String> {
        let parts = self.parts().filter(|parts| parts.total > 0.0)?;
        let uncached = self
            .input_tokens
            .saturating_sub(self.cache_read_tokens)
            .saturating_sub(self.cache_write_tokens);
        let line = [
            ("cache read", parts.cache_read, self.cache_read_tokens),
            ("cache write", parts.cache_write, self.cache_write_tokens),
            ("uncached input", parts.uncached_input, uncached),
            ("output", parts.output, self.output_tokens),
        ]
        .map(|(name, dollars, tokens)| {
            format!(
                "{name} {} ({:.0}%, {} tok)",
                display_cost(Some(dollars)),
                dollars / parts.total * 100.0,
                format_tokens(tokens)
            )
        })
        .join(" · ");
        Some(line)
    }

    /// The session's input and output tokens so far.
    #[must_use]
    pub const fn tokens(&self) -> (u64, u64) {
        (self.input_tokens, self.output_tokens)
    }

    /// The session's cost so far in US dollars (0 when it is not priced).
    #[must_use]
    pub const fn total_usd(&self) -> f64 {
        if self.unpriced_usage {
            0.0
        } else {
            self.total_usd
        }
    }

    /// What the context held at the last response, when there was one.
    #[must_use]
    pub const fn context_tokens(&self) -> Option<u64> {
        self.context_tokens
    }

    /// How much of the session's prompt tokens the provider read from its cache,
    /// as a percentage: `87%`. The share a session pays the cache price for is
    /// the one number that says whether its prefix stays still; Claude Code's
    /// team treats a drop in it as an incident. `None` before any usage.
    #[must_use]
    pub fn cache_label(&self) -> Option<String> {
        if self.input_tokens == 0 {
            return None;
        }
        let percent = self.cache_read_tokens.saturating_mul(100) / self.input_tokens;
        Some(format!("{}%", percent.min(100)))
    }

    /// A new conversation starts with an empty context.
    pub fn forget_context(&mut self) {
        self.context_tokens = None;
        self.estimate_scale = None;
    }

    /// After a compaction the last response's usage measured a context that
    /// is gone: pi shows `?` until a response measures the new one.
    pub fn context_compacted(&mut self) {
        self.context_tokens = None;
    }

    #[must_use]
    pub const fn estimate_scale(&self) -> Option<f64> {
        self.estimate_scale
    }

    pub const fn set_estimate_scale(&mut self, scale: f64) {
        self.estimate_scale = Some(scale);
    }

    #[must_use]
    pub fn display(&self) -> String {
        if !self.has_usage || self.unpriced_usage {
            "n/a".to_owned()
        } else {
            display_cost(Some(self.total_usd))
        }
    }
}

/// Return `None` when the selected model has no complete price configuration.
pub fn calculate_cost(price: Option<ModelPrice>, usage: Usage) -> Option<f64> {
    split_cost(price, usage).map(|parts| parts.total)
}

/// One call's cost by the kind of token: the prompt that was not cached, what
/// was read from the cache, what was written to it, and the output. A cache
/// price that is not set is the input price. `None` when the model has no
/// price. `calculate_cost` is this function's `total`.
pub fn split_cost(price: Option<ModelPrice>, usage: Usage) -> Option<CostParts> {
    let price = price?;
    let tokens = |count: u64| u32::try_from(count).ok().map(f64::from);
    let uncached = usage
        .input_tokens
        .saturating_sub(usage.cache_read_tokens)
        .saturating_sub(usage.cache_write_tokens);
    // Each part times a million, so `total` is summed before its one
    // division, as the cost always was.
    let raw = [
        tokens(uncached)? * price.input_per_mtok,
        tokens(usage.cache_read_tokens)?
            * price.cache_read_per_mtok.unwrap_or(price.input_per_mtok),
        tokens(usage.cache_write_tokens)?
            * price.cache_write_per_mtok.unwrap_or(price.input_per_mtok),
        tokens(usage.output_tokens)? * price.output_per_mtok,
    ];
    let parts = CostParts {
        uncached_input: raw[0] / 1_000_000.0,
        cache_read: raw[1] / 1_000_000.0,
        cache_write: raw[2] / 1_000_000.0,
        output: raw[3] / 1_000_000.0,
        total: (raw[0] + raw[1] + raw[2] + raw[3]) / 1_000_000.0,
    };
    [
        parts.uncached_input,
        parts.cache_read,
        parts.cache_write,
        parts.output,
        parts.total,
    ]
    .iter()
    .all(|part| part.is_finite())
    .then_some(parts)
}

#[must_use]
pub fn display_cost(cost: Option<f64>) -> String {
    cost.map_or_else(|| "n/a".to_owned(), |value| format!("${value:.4}"))
}

/// `950`, `12k`, `1.2M`, `1M`: prime-agent's token counts.
#[must_use]
pub fn format_tokens(tokens: u64) -> String {
    #[allow(clippy::cast_precision_loss, reason = "a rounded label")]
    let value = tokens as f64;
    if tokens >= 1_000_000 {
        let millions = value / 1_000_000.0;
        if tokens.is_multiple_of(1_000_000) {
            format!("{millions:.0}M")
        } else {
            format!("{millions:.1}M")
        }
    } else if tokens >= 1_000 {
        format!("{:.0}k", value / 1_000.0)
    } else {
        tokens.to_string()
    }
}

/// How full the context window is: `12% · 123k/1M`.
#[must_use]
pub fn context_label(used: u64, window: u64) -> String {
    if window == 0 {
        return format_tokens(used);
    }
    #[allow(clippy::cast_precision_loss, reason = "a rounded percentage")]
    let percent = used as f64 / window as f64 * 100.0;
    format!(
        "{percent:.0}% · {}/{}",
        format_tokens(used),
        format_tokens(window)
    )
}

#[cfg(test)]
mod tests {
    use super::{
        CostTracker, ModelPrice, Usage, calculate_cost, context_label, format_tokens, split_cost,
    };

    /// The cache share is the session's prompt tokens read from the cache.
    #[test]
    fn the_cache_share_is_read_tokens_over_prompt_tokens() {
        let mut tracker = super::CostTracker::default();
        assert_eq!(tracker.cache_label(), None);
        tracker.record(
            None,
            super::Usage {
                input_tokens: 1_000,
                output_tokens: 10,
                cache_read_tokens: 0,
                cache_write_tokens: 1_000,
            },
        );
        assert_eq!(tracker.cache_label().as_deref(), Some("0%"));
        tracker.record(
            None,
            super::Usage {
                input_tokens: 3_000,
                output_tokens: 10,
                cache_read_tokens: 2_000,
                cache_write_tokens: 0,
            },
        );
        assert_eq!(tracker.cache_label().as_deref(), Some("50%"));
    }

    #[test]
    fn token_counts_read_like_prime_agents() {
        assert_eq!(format_tokens(950), "950");
        assert_eq!(format_tokens(12_400), "12k");
        assert_eq!(format_tokens(1_000_000), "1M");
        assert_eq!(format_tokens(1_250_000), "1.2M");
        assert_eq!(context_label(123_000, 1_000_000), "12% · 123k/1M");
    }

    fn near(left: f64, right: f64) -> bool {
        (left - right).abs() < 1e-12
    }

    fn cached_usage() -> Usage {
        Usage {
            input_tokens: 1_000_000,
            output_tokens: 70_000,
            cache_read_tokens: 800_000,
            cache_write_tokens: 100_000,
        }
    }

    /// The four parts add up to `calculate_cost`, priced with a cache price
    /// or without one; an unpriced model has no parts.
    #[test]
    fn the_split_sums_to_calculate_cost() {
        let full = ModelPrice {
            input_per_mtok: 5.0,
            output_per_mtok: 30.0,
            cache_read_per_mtok: Some(0.5),
            cache_write_per_mtok: Some(6.25),
        };
        let parts = split_cost(Some(full), cached_usage()).expect("priced");
        assert!(near(parts.uncached_input, 0.1 * 5.0), "{parts:?}");
        assert!(near(parts.cache_read, 0.8 * 0.5), "{parts:?}");
        assert!(near(parts.cache_write, 0.1 * 6.25), "{parts:?}");
        assert!(near(parts.output, 0.07 * 30.0), "{parts:?}");
        let sum = parts.uncached_input + parts.cache_read + parts.cache_write + parts.output;
        assert!(near(sum, parts.total), "{parts:?}");
        assert_eq!(
            calculate_cost(Some(full), cached_usage()),
            Some(parts.total)
        );

        // No cache price: reads and writes cost the input price.
        let bare = ModelPrice {
            cache_read_per_mtok: None,
            cache_write_per_mtok: None,
            ..full
        };
        let parts = split_cost(Some(bare), cached_usage()).expect("priced");
        assert!(near(parts.cache_read, 0.8 * 5.0), "{parts:?}");
        assert!(near(parts.cache_write, 0.1 * 5.0), "{parts:?}");
        assert_eq!(
            calculate_cost(Some(bare), cached_usage()),
            Some(parts.total)
        );

        // A prompt smaller than its cache counts saturates at no uncached input.
        let odd = Usage {
            input_tokens: 10,
            cache_read_tokens: 20,
            ..Usage::default()
        };
        assert!(near(
            split_cost(Some(full), odd).expect("priced").uncached_input,
            0.0
        ));

        assert_eq!(split_cost(None, cached_usage()), None);
        assert_eq!(calculate_cost(None, cached_usage()), None);
    }

    /// The tracker keeps the parts; `/cost` prints them with shares and tokens.
    #[test]
    fn the_tracker_keeps_the_parts_and_prints_them() {
        let price = ModelPrice {
            input_per_mtok: 5.0,
            output_per_mtok: 30.0,
            cache_read_per_mtok: Some(0.5),
            cache_write_per_mtok: Some(6.25),
        };
        let mut tracker = CostTracker::default();
        assert_eq!(tracker.parts(), None);
        assert_eq!(tracker.breakdown(), None);
        tracker.record(Some(price), cached_usage());
        tracker.record(Some(price), cached_usage());
        let parts = tracker.parts().expect("priced");
        assert!(near(parts.total, tracker.total_usd()), "{parts:?}");
        assert!(near(parts.cache_read, 2.0 * 0.8 * 0.5), "{parts:?}");
        // 2.0M prompt tokens: 1.6M read, 200k written, 200k uncached; 140k out.
        let line = tracker.breakdown().expect("a breakdown");
        assert_eq!(
            line,
            "cache read $0.8000 (11%, 1.6M tok) · cache write $1.2500 (17%, 200k tok) \
             · uncached input $1.0000 (14%, 200k tok) · output $4.2000 (58%, 140k tok)"
        );
        // The first line of `/cost` is the total, unchanged.
        assert_eq!(tracker.display(), "$7.2500");
        // One unpriced call takes the parts away with the total.
        tracker.record(None, Usage::default());
        assert_eq!(tracker.parts(), None);
        assert_eq!(tracker.breakdown(), None);
        assert_eq!(tracker.display(), "n/a");
    }

    #[test]
    fn g03_cost_uses_config_prices_or_na() {
        let cost = calculate_cost(
            Some(ModelPrice {
                input_per_mtok: 3.0,
                output_per_mtok: 6.0,
                cache_read_per_mtok: None,
                cache_write_per_mtok: None,
            }),
            Usage {
                input_tokens: 1_000,
                output_tokens: 2_000,
                ..Usage::default()
            },
        );
        assert_eq!(cost, Some(0.015));
        assert_eq!(calculate_cost(None, Usage::default()), None);
    }
}
