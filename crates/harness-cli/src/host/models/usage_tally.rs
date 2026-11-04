//! The tokens a set of model calls used, counted where the calls are made:
//! `ha exec` reports its own calls, its children's and the background memory
//! passes' apart (`usage`, `children_usage`, `upkeep_usage` in its JSON).
//!
//! A provider wrapped in [`Tallied`] adds each call's usage to its
//! [`UsageTally`] once the call is done. A call reports its usage in a
//! `Usage` event; a provider that reports it twice (before and after the
//! completion) is counted once, by the last report.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::StreamExt as _;
use harness_providers::{
    CancellationToken, ModelCapabilities, ModelProvider, ProviderEventStream, ProviderFuture,
    ProviderRequest, ProviderStreamEvent,
};

/// What a set of model calls used. `input_tokens` counts every prompt token;
/// of them, `cached_input_tokens` were read from the provider's prompt cache
/// and `cache_write_tokens` written to it. `model_calls` counts the calls
/// that reported their usage.
#[derive(Debug, Default)]
pub struct UsageTally {
    input: AtomicU64,
    cached: AtomicU64,
    cache_write: AtomicU64,
    output: AtomicU64,
    calls: AtomicU64,
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
    /// Add one call's usage.
    pub fn add(&self, usage: CallUsage) {
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

    /// The tally as `ha exec`'s JSON reports it.
    #[must_use]
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "input_tokens": self.input.load(Ordering::SeqCst),
            "cached_input_tokens": self.cached.load(Ordering::SeqCst),
            "cache_write_tokens": self.cache_write.load(Ordering::SeqCst),
            "output_tokens": self.output.load(Ordering::SeqCst),
            "model_calls": self.calls(),
        })
    }
}

/// A provider whose calls are counted in a [`UsageTally`].
pub struct Tallied {
    inner: Arc<dyn ModelProvider>,
    tally: Arc<UsageTally>,
}

impl Tallied {
    /// `inner`, its calls counted in `tally`.
    #[must_use]
    pub fn wrap(inner: Arc<dyn ModelProvider>, tally: &Arc<UsageTally>) -> Arc<dyn ModelProvider> {
        Arc::new(Self {
            inner,
            tally: Arc::clone(tally),
        })
    }
}

/// The last usage one call reported, added to the tally when the call's
/// stream is dropped: done, failed or cut off.
struct Call {
    tally: Arc<UsageTally>,
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
            self.tally.add(usage);
        }
    }
}

impl ModelProvider for Tallied {
    fn capabilities(&self) -> ModelCapabilities {
        self.inner.capabilities()
    }

    fn stream(&self, request: ProviderRequest, cancellation: CancellationToken) -> ProviderFuture {
        let future = self.inner.stream(request, cancellation);
        let tally = Arc::clone(&self.tally);
        Box::pin(async move {
            let events = future.await?;
            if let Some(usage) = events.iter().rev().find_map(CallUsage::of) {
                tally.add(usage);
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

    use super::{Tallied, UsageTally};

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
            })
        );
    }

    /// A call that reports no usage is not counted.
    #[tokio::test]
    async fn a_call_with_no_usage_is_not_a_counted_call() {
        let tally = Arc::new(UsageTally::default());
        let provider = Tallied::wrap(Arc::new(MockProvider::text("no usage")), &tally);
        let mut stream = provider.stream_events(request(), CancellationToken::new());
        while stream.next().await.is_some() {}
        drop(stream);
        assert_eq!(tally.calls(), 0);
        assert_eq!(tally.json()["input_tokens"], 0);
    }
}
