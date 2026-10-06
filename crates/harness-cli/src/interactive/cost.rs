//! Session cost calculations from configured per-million token prices.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelPrice {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    /// prime-agent prices cached input apart: a read from the cache and a
    /// write to it. `None` prices them as ordinary input.
    pub cache_read_per_mtok: Option<f64>,
    pub cache_write_per_mtok: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Of `input_tokens`, read from and written to the prompt cache.
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CostTracker {
    total_usd: f64,
    has_usage: bool,
    unpriced_usage: bool,
    input_tokens: u64,
    output_tokens: u64,
    /// The last request's prompt plus its answer: what the context holds now, as
    /// prime-agent reads it off the latest response's usage.
    context_tokens: Option<u64>,
}

impl CostTracker {
    pub fn record(&mut self, price: Option<ModelPrice>, usage: Usage) {
        self.has_usage = true;
        self.input_tokens = self.input_tokens.saturating_add(usage.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(usage.output_tokens);
        self.context_tokens = Some(usage.input_tokens.saturating_add(usage.output_tokens));
        match calculate_cost(price, usage) {
            Some(cost) if !self.unpriced_usage => self.total_usd += cost,
            Some(_) => {}
            None => self.unpriced_usage = true,
        }
    }

    /// The session's input and output tokens so far.
    #[must_use]
    pub const fn tokens(&self) -> (u64, u64) {
        (self.input_tokens, self.output_tokens)
    }

    /// What the context held at the last response, when there was one.
    #[must_use]
    pub const fn context_tokens(&self) -> Option<u64> {
        self.context_tokens
    }

    /// A new conversation starts with an empty context.
    pub fn forget_context(&mut self) {
        self.context_tokens = None;
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
    let price = price?;
    let tokens = |count: u64| u32::try_from(count).ok().map(f64::from);
    let uncached = usage
        .input_tokens
        .saturating_sub(usage.cache_read_tokens)
        .saturating_sub(usage.cache_write_tokens);
    let total = (tokens(uncached)? * price.input_per_mtok
        + tokens(usage.cache_read_tokens)?
            * price.cache_read_per_mtok.unwrap_or(price.input_per_mtok)
        + tokens(usage.cache_write_tokens)?
            * price.cache_write_per_mtok.unwrap_or(price.input_per_mtok)
        + tokens(usage.output_tokens)? * price.output_per_mtok)
        / 1_000_000.0;
    total.is_finite().then_some(total)
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
    use super::{ModelPrice, Usage, calculate_cost, context_label, format_tokens};

    #[test]
    fn token_counts_read_like_prime_agents() {
        assert_eq!(format_tokens(950), "950");
        assert_eq!(format_tokens(12_400), "12k");
        assert_eq!(format_tokens(1_000_000), "1M");
        assert_eq!(format_tokens(1_250_000), "1.2M");
        assert_eq!(context_label(123_000, 1_000_000), "12% · 123k/1M");
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
