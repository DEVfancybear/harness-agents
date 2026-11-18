//! The tokens a set of model calls used, counted where the calls are made:
//! `ha exec` reports its own calls, its children's and the background memory
//! passes' apart (`usage`, `children_usage`, `upkeep_usage` in its JSON).
//!
//! A provider wrapped in [`Tallied`] adds each call's usage to its
//! [`UsageTally`] once the call is done. A call reports its usage in a
//! `Usage` event; a provider that reports it twice (before and after the
//! completion) is counted once, by the last report.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use futures_util::StreamExt as _;
use harness_providers::{
    CancellationToken, ModelCapabilities, ModelProvider, ProviderEventStream, ProviderFuture,
    ProviderRequest, ProviderStreamEvent,
};

use super::cost::{CostParts, ModelPrice, Usage, split_cost};

/// The price of the model a call was made with, by the model id the request
/// names. `None` is a model that has no price.
pub type PriceLookup = Arc<dyn Fn(&str) -> Option<ModelPrice> + Send + Sync>;

/// What a set of model calls used. `input_tokens` counts every prompt token;
/// of them, `cached_input_tokens` were read from the provider's prompt cache
/// and `cache_write_tokens` written to it. `model_calls` counts the calls
/// that reported their usage. What they cost in dollars is kept by kind of
/// token, each call at the price of the model it was made with.
#[derive(Debug, Default)]
pub struct UsageTally {
    input: AtomicU64,
    cached: AtomicU64,
    cache_write: AtomicU64,
    output: AtomicU64,
    calls: AtomicU64,
    spend: Mutex<Spend>,
}

/// The dollars of the counted calls; one call without a price spoils them.
#[derive(Debug, Default)]
struct Spend {
    parts: CostParts,
    unpriced: bool,
}

/// One call's usage, as its `Usage` event reports it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CallUsage {
    pub input: u64,
    pub cached: u64,
    pub cache_write: u64,
    pub output: u64,
}

impl CallUsage {
    /// The usage `event` reports, when it is a `Usage` event.
    #[must_use]
    pub fn of(event: &ProviderStreamEvent) -> Option<Self> {
        match event {
            ProviderStreamEvent::Usage {
                prompt_tokens,
                completion_tokens,
                cache_read_tokens,
                cache_write_tokens,
                ..
            } => Some(Self {
                input: *prompt_tokens,
                cached: *cache_read_tokens,
                cache_write: *cache_write_tokens,
                output: *completion_tokens,
            }),
            _ => None,
        }
    }
}

impl UsageTally {
    /// Add one call's usage and, at `price`, its dollars; with none, the dollars
    /// of the tally are not known.
    pub fn add_priced(&self, usage: CallUsage, price: Option<ModelPrice>) {
        let split = split_cost(
            price,
            Usage {
                input_tokens: usage.input,
                output_tokens: usage.output,
                cache_read_tokens: usage.cached,
                cache_write_tokens: usage.cache_write,
            },
        );
        {
            let mut spend = self.spend.lock().unwrap_or_else(PoisonError::into_inner);
            match split {
                Some(parts) => spend.parts = spend.parts.plus(parts),
                None => spend.unpriced = true,
            }
        }
        self.input.fetch_add(usage.input, Ordering::SeqCst);
        self.cached.fetch_add(usage.cached, Ordering::SeqCst);
        self.cache_write
            .fetch_add(usage.cache_write, Ordering::SeqCst);
        self.output.fetch_add(usage.output, Ordering::SeqCst);
        self.calls.fetch_add(1, Ordering::SeqCst);
    }

    /// The calls counted so far.
    #[must_use]
    pub fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }

    /// What the counted calls cost; nothing when none was counted, `None`
    /// when one of them was made with a model that has no price.
    #[must_use]
    pub fn spend(&self) -> Option<CostParts> {
        let spend = self.spend.lock().unwrap_or_else(PoisonError::into_inner);
        (!spend.unpriced).then_some(spend.parts)
    }

    /// The tally as `ha exec`'s JSON reports it.
    #[must_use]
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "input_tokens": self.input.load(Ordering::SeqCst),
            "cached_input_tokens": self.cached.load(Ordering::SeqCst),
            "cache_write_tokens": self.cache_write.load(Ordering::SeqCst),
            "output_tokens": self.output.load(Ordering::SeqCst),
            "model_calls": self.calls(),
            "cost_usd": cost_json(self.spend().filter(|_| self.calls() > 0)),
        })
    }
}

/// The dollars of several tallies together: `None` when one of them had a
/// call without a price, or when none counted a call (a tally that counted
/// none adds nothing).
#[must_use]
pub fn combined_spend(tallies: &[&UsageTally]) -> Option<CostParts> {
    if tallies.iter().all(|tally| tally.calls() == 0) {
        return None;
    }
    tallies.iter().try_fold(CostParts::default(), |sum, tally| {
        tally.spend().map(|parts| sum.plus(parts))
    })
}

/// `ha exec`'s `cost_usd`: the four kinds of token and their total in US
/// dollars, to a millionth of one; `null` when it is not known.
#[must_use]
pub fn cost_json(parts: Option<CostParts>) -> serde_json::Value {
    let micro = |dollars: f64| (dollars * 1_000_000.0).round() / 1_000_000.0;
    parts.map_or(serde_json::Value::Null, |parts| {
        serde_json::json!({
            "uncached_input": micro(parts.uncached_input),
            "cache_read": micro(parts.cache_read),
            "cache_write": micro(parts.cache_write),
            "output": micro(parts.output),
            "total": micro(parts.total),
        })
    })
}

/// The prices the catalog and the config give, by the id of the model a
/// request names. A price in the config wins; the model the run is on
/// (`provider`, `model`, `price`) has the price it was resolved with; any
/// other model - a child's on `/subagent-model` or `SUBAGENT_FROM` - is the
/// catalog's, under the run's provider first and else by its id alone.
#[must_use]
pub fn catalog_prices(
    configured: BTreeMap<String, ModelPrice>,
    run: Option<(String, String, Option<ModelPrice>)>,
    data_dir: &Path,
) -> PriceLookup {
    let catalog = super::providers::Catalog::load(data_dir);
    Arc::new(move |model: &str| {
        if let Some(price) = configured.get(model) {
            return Some(*price);
        }
        if let Some((provider, run_model, price)) = &run {
            if run_model == model {
                return *price;
            }
            if let Some(found) = catalog.find(&format!("{provider}/{model}")) {
                return found.cost.as_ref().and_then(ModelPrice::of_catalog);
            }
        }
        catalog
            .find(model)
            .and_then(|found| found.cost.as_ref())
            .and_then(ModelPrice::of_catalog)
    })
}

/// A provider whose calls are counted in a [`UsageTally`].
pub struct Tallied {
    inner: Arc<dyn ModelProvider>,
    tally: Arc<UsageTally>,
    prices: PriceLookup,
}

impl Tallied {
    /// `inner`, its calls counted in `tally` with their dollars, each at the
    /// price `prices` gives for the model the call was made with.
    #[must_use]
    pub fn wrap(
        inner: Arc<dyn ModelProvider>,
        tally: &Arc<UsageTally>,
        prices: &PriceLookup,
    ) -> Arc<dyn ModelProvider> {
        Arc::new(Self {
            inner,
            tally: Arc::clone(tally),
            prices: Arc::clone(prices),
        })
    }

    /// The price of the model `request` names.
    fn price_of(&self, request: &ProviderRequest) -> Option<ModelPrice> {
        (self.prices)(&request.model)
    }
}

/// The last usage one call reported, added to the tally when the call's
/// stream is dropped: done, failed or cut off.
struct Call {
    tally: Arc<UsageTally>,
    price: Option<ModelPrice>,
    last: Option<CallUsage>,
}

impl Call {
    /// One event of the call: a usage report replaces the one before it.
    fn saw(&mut self, event: &Result<ProviderStreamEvent, harness_providers::ProviderError>) {
        if let Ok(event) = event
            && let Some(usage) = CallUsage::of(event)
        {
            self.last = Some(usage);
        }
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        if let Some(usage) = self.last.take() {
            self.tally.add_priced(usage, self.price);
        }
    }
}

impl ModelProvider for Tallied {
    fn capabilities(&self) -> ModelCapabilities {
        self.inner.capabilities()
    }

    fn stream(&self, request: ProviderRequest, cancellation: CancellationToken) -> ProviderFuture {
        let price = self.price_of(&request);
        let future = self.inner.stream(request, cancellation);
        let tally = Arc::clone(&self.tally);
        Box::pin(async move {
            let events = future.await?;
            if let Some(usage) = events.iter().rev().find_map(CallUsage::of) {
                tally.add_priced(usage, price);
            }
            Ok(events)
        })
    }

    fn stream_events(
        &self,
        request: ProviderRequest,
        cancellation: CancellationToken,
    ) -> ProviderEventStream {
        let mut call = Call {
            tally: Arc::clone(&self.tally),
            price: self.price_of(&request),
            last: None,
        };
        // The call is moved into the stream whole, so it is dropped - and
        // counted - with the stream.
        Box::pin(
            self.inner
                .stream_events(request, cancellation)
                .inspect(move |event| call.saw(event)),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use futures_util::StreamExt as _;
    use harness_providers::{
        CancellationToken, MockProvider, ProviderRequest, ProviderStreamEvent,
    };
    use harness_types::RequestId;

    use std::collections::BTreeMap;

    use super::{PriceLookup, Tallied, UsageTally, catalog_prices, combined_spend, cost_json};
    use crate::interactive::cost::ModelPrice;

    fn usage(prompt: u64, completion: u64, read: u64, write: u64) -> ProviderStreamEvent {
        ProviderStreamEvent::Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
            cache_read_tokens: read,
            cache_write_tokens: write,
        }
    }

    fn request() -> ProviderRequest {
        ProviderRequest::new(RequestId::generate(), "m", Vec::new())
    }

    /// Each call is counted once, by its last report, through both shapes
    /// of call.
    #[tokio::test]
    async fn every_call_is_counted_once_with_its_cache_tokens() {
        let tally = Arc::new(UsageTally::default());
        // OpenCode reports the usage before and after the completion.
        let provider = Tallied::wrap(
            Arc::new(MockProvider::scripted(vec![
                ProviderStreamEvent::started(),
                ProviderStreamEvent::text("hi"),
                usage(90, 5, 0, 0),
                ProviderStreamEvent::completed("stop"),
                usage(100, 7, 60, 40),
            ])),
            &tally,
            &no_prices(),
        );
        let events = provider
            .stream(request(), CancellationToken::new())
            .await
            .expect("events");
        assert!(!events.is_empty());
        let mut stream = provider.stream_events(request(), CancellationToken::new());
        while stream.next().await.is_some() {}
        drop(stream);
        assert_eq!(
            tally.json(),
            serde_json::json!({
                "input_tokens": 200,
                "cached_input_tokens": 120,
                "cache_write_tokens": 80,
                "output_tokens": 14,
                "model_calls": 2,
                "cost_usd": null,
            })
        );
    }

    /// A call that reports no usage is not counted.
    #[tokio::test]
    async fn a_call_with_no_usage_is_not_a_counted_call() {
        let tally = Arc::new(UsageTally::default());
        let provider = Tallied::wrap(
            Arc::new(MockProvider::text("no usage")),
            &tally,
            &no_prices(),
        );
        let mut stream = provider.stream_events(request(), CancellationToken::new());
        while stream.next().await.is_some() {}
        drop(stream);
        assert_eq!(tally.calls(), 0);
        assert_eq!(tally.json()["input_tokens"], 0);
    }

    fn no_prices() -> PriceLookup {
        Arc::new(|_: &str| None)
    }

    fn model_request(model: &str) -> ProviderRequest {
        ProviderRequest::new(RequestId::generate(), model, Vec::new())
    }

    fn lookup() -> PriceLookup {
        Arc::new(|model: &str| match model {
            "cheap" => Some(ModelPrice {
                input_per_mtok: 1.0,
                output_per_mtok: 4.0,
                cache_read_per_mtok: Some(0.1),
                cache_write_per_mtok: Some(1.25),
            }),
            "dear" => Some(ModelPrice {
                input_per_mtok: 10.0,
                output_per_mtok: 40.0,
                cache_read_per_mtok: Some(1.0),
                cache_write_per_mtok: Some(12.5),
            }),
            _ => None,
        })
    }

    fn scripted(prompt: u64, completion: u64, read: u64, write: u64) -> Arc<MockProvider> {
        Arc::new(MockProvider::scripted(vec![
            ProviderStreamEvent::started(),
            usage(prompt, completion, read, write),
            ProviderStreamEvent::completed("stop"),
        ]))
    }

    fn assert_near(left: f64, right: f64) {
        assert!((left - right).abs() < 1e-9, "{left} != {right}");
    }

    fn field(json: &serde_json::Value, name: &str) -> f64 {
        json["cost_usd"][name].as_f64().expect(name)
    }

    /// Two calls on two models, one of each shape, are each priced at their
    /// own model's price and kept by kind of token.
    #[tokio::test]
    async fn dollars_are_kept_by_kind_at_the_price_of_each_calls_model() {
        let tally = Arc::new(UsageTally::default());
        let prices = lookup();
        let cheap = Tallied::wrap(
            scripted(1_000_000, 100_000, 600_000, 100_000),
            &tally,
            &prices,
        );
        let dear = Tallied::wrap(scripted(100_000, 10_000, 50_000, 0), &tally, &prices);
        cheap
            .stream(model_request("cheap"), CancellationToken::new())
            .await
            .expect("events");
        let mut stream = dear.stream_events(model_request("dear"), CancellationToken::new());
        while stream.next().await.is_some() {}
        drop(stream);
        let json = tally.json();
        assert_eq!(json["model_calls"], 2);
        assert_eq!(json["input_tokens"], 1_100_000);
        // cheap: 0.3 + 0.06 + 0.125 + 0.4; dear: 0.5 + 0.05 + 0 + 0.4.
        assert_near(field(&json, "uncached_input"), 0.8);
        assert_near(field(&json, "cache_read"), 0.11);
        assert_near(field(&json, "cache_write"), 0.125);
        assert_near(field(&json, "output"), 0.8);
        assert_near(field(&json, "total"), 1.835);
    }

    /// One call on a model with no price makes the tally's dollars unknown,
    /// as does a provider wrapped with no prices; no call is no dollars.
    #[tokio::test]
    async fn an_unpriced_call_makes_the_dollars_null() {
        let prices = lookup();
        let tally = Arc::new(UsageTally::default());
        assert!(tally.json()["cost_usd"].is_null(), "no call counted");
        let known = Tallied::wrap(scripted(1_000, 10, 0, 0), &tally, &prices);
        known
            .stream(model_request("cheap"), CancellationToken::new())
            .await
            .expect("events");
        assert!(tally.json()["cost_usd"].is_object());
        known
            .stream(model_request("nobody"), CancellationToken::new())
            .await
            .expect("events");
        let json = tally.json();
        assert_eq!(json["model_calls"], 2);
        assert!(json["cost_usd"].is_null(), "{json}");
        assert_eq!(tally.spend(), None);

        let bare = Arc::new(UsageTally::default());
        Tallied::wrap(scripted(1_000, 10, 0, 0), &bare, &no_prices())
            .stream(model_request("cheap"), CancellationToken::new())
            .await
            .expect("events");
        assert!(bare.json()["cost_usd"].is_null());
    }

    /// The run's dollars are its tallies' together; a tally that counted no
    /// call adds nothing, one with an unpriced call makes the sum unknown.
    #[tokio::test]
    async fn tallies_add_up_unless_one_is_unpriced() {
        let prices = lookup();
        let (run, children, upkeep) = (
            Arc::new(UsageTally::default()),
            Arc::new(UsageTally::default()),
            Arc::new(UsageTally::default()),
        );
        assert_eq!(combined_spend(&[&run, &children, &upkeep]), None);
        for (tally, model) in [(&run, "cheap"), (&children, "dear")] {
            Tallied::wrap(scripted(1_000_000, 0, 0, 0), tally, &prices)
                .stream(model_request(model), CancellationToken::new())
                .await
                .expect("events");
        }
        let sum = combined_spend(&[&run, &children, &upkeep]).expect("priced");
        assert!((sum.total - 11.0).abs() < 1e-9, "{sum:?}");
        assert!((sum.uncached_input - 11.0).abs() < 1e-9, "{sum:?}");
        assert_eq!(cost_json(Some(sum))["total"], 11.0);
        Tallied::wrap(scripted(1_000, 0, 0, 0), &upkeep, &prices)
            .stream(model_request("nobody"), CancellationToken::new())
            .await
            .expect("events");
        assert_eq!(combined_spend(&[&run, &children, &upkeep]), None);
        assert!(cost_json(None).is_null());
    }

    /// A model the config prices is priced from the config; the run's model
    /// has the price it resolved with; another is the catalog's, or none.
    #[test]
    fn catalog_prices_look_a_model_up_by_its_id() {
        let configured = BTreeMap::from([(
            "mine".to_owned(),
            ModelPrice {
                input_per_mtok: 2.0,
                output_per_mtok: 3.0,
                cache_read_per_mtok: None,
                cache_write_per_mtok: None,
            },
        )]);
        let run_price = ModelPrice {
            input_per_mtok: 7.0,
            output_per_mtok: 8.0,
            cache_read_per_mtok: Some(0.7),
            cache_write_per_mtok: None,
        };
        let data_dir = std::env::temp_dir().join("ha-catalog-prices-test-missing");
        let lookup = catalog_prices(
            configured,
            Some((
                "nowhere".to_owned(),
                "run-model".to_owned(),
                Some(run_price),
            )),
            &data_dir,
        );
        assert_eq!(lookup("mine").map(|price| price.input_per_mtok), Some(2.0));
        assert_eq!(lookup("run-model"), Some(run_price));
        assert_eq!(lookup("no-such-model-anywhere"), None);
        // A model of the bundled catalog is priced, if the catalog prices it.
        let listed = crate::interactive::providers::Catalog::bundled();
        let priced = listed
            .models()
            .iter()
            .find(|model| {
                model.cost.is_some_and(|cost| cost.input > 0.0)
                    && listed.find(&model.id).is_some_and(|found| found == *model)
            })
            .expect("a priced model in the catalog");
        assert!(lookup(&priced.id).is_some(), "{}", priced.id);
    }
}
