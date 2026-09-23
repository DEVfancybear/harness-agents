//! Session cost calculations from configured per-million token prices.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelPrice {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CostTracker {
    total_usd: f64,
    has_usage: bool,
    unpriced_usage: bool,
}

impl CostTracker {
    pub fn record(&mut self, price: Option<ModelPrice>, usage: Usage) {
        self.has_usage = true;
        match calculate_cost(price, usage) {
            Some(cost) if !self.unpriced_usage => self.total_usd += cost,
            Some(_) => {}
            None => self.unpriced_usage = true,
        }
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
    let input_tokens = f64::from(u32::try_from(usage.input_tokens).ok()?);
    let output_tokens = f64::from(u32::try_from(usage.output_tokens).ok()?);
    let total =
        (input_tokens * price.input_per_mtok + output_tokens * price.output_per_mtok) / 1_000_000.0;
    total.is_finite().then_some(total)
}

#[must_use]
pub fn display_cost(cost: Option<f64>) -> String {
    cost.map_or_else(|| "n/a".to_owned(), |value| format!("${value:.4}"))
}

#[cfg(test)]
mod tests {
    use super::{ModelPrice, Usage, calculate_cost};

    #[test]
    fn g03_cost_uses_config_prices_or_na() {
        let cost = calculate_cost(
            Some(ModelPrice {
                input_per_mtok: 3.0,
                output_per_mtok: 6.0,
            }),
            Usage {
                input_tokens: 1_000,
                output_tokens: 2_000,
            },
        );
        assert_eq!(cost, Some(0.015));
        assert_eq!(calculate_cost(None, Usage::default()), None);
    }
}
