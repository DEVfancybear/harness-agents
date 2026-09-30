//! Which models a session cycles through and hands work to, after prime-agent's
//! scoped models (`enabledModels`), its auxiliary, backup and image models, and
//! its wait for provider usage to recover (`retry.provider.waitForUsage`).

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use harness_providers::{
    CancellationToken, ModelProvider, ProviderError, ProviderEventStream, ProviderFuture,
    ProviderRequest, ProviderStreamEvent, ThinkingLevel,
};
use harness_types::ErrorCode;
use tokio::sync::mpsc::UnboundedSender;

use super::events::SessionEvent;

use super::paths::LaunchEnvironment;
use super::providers::{Catalog, Model};
use super::service::ProviderConfig;

/// One model `/model next` and Alt+M cycle through.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopeEntry {
    /// `provider/id`.
    pub reference: String,
    /// The thinking level the pattern asked for with `:level`.
    pub level: Option<ThinkingLevel>,
}

/// A pattern and the level its `:level` suffix names, when the suffix is one.
fn split_level(pattern: &str) -> (&str, Option<ThinkingLevel>) {
    if let Some((model, suffix)) = pattern.rsplit_once(':')
        && let Some(level) = ThinkingLevel::parse(suffix)
    {
        return (model, Some(level));
    }
    (pattern, None)
}

/// A glob with `*` and `?`, in any case.
fn glob(pattern: &str, text: &str) -> bool {
    let pattern = pattern.to_lowercase().chars().collect::<Vec<_>>();
    let text = text.to_lowercase().chars().collect::<Vec<_>>();
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, t));
            p += 1;
        } else if let Some((star_p, star_t)) = star {
            p = star_p + 1;
            t = star_t + 1;
            star = Some((star_p, star_t + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|char| *char == '*')
}

/// Whether a pattern names this model, by `provider/id` or by id.
fn matches(pattern: &str, model: &Model) -> bool {
    glob(pattern, &model.reference()) || glob(pattern, &model.id)
}

/// The models the patterns name, in pattern order then catalog order, each once.
#[must_use]
pub fn scope(patterns: &[String], models: &[Model]) -> Vec<ScopeEntry> {
    let mut entries: Vec<ScopeEntry> = Vec::new();
    for pattern in patterns {
        let (pattern, level) = split_level(pattern.trim());
        for model in models.iter().filter(|model| matches(pattern, model)) {
            let reference = model.reference();
            if !entries.iter().any(|entry| entry.reference == reference) {
                entries.push(ScopeEntry { reference, level });
            }
        }
    }
    entries
}

/// The entry after (or before) the current model, wrapping around. A current
/// model outside the scope moves to its first (or last) entry.
#[must_use]
pub fn step<'a>(entries: &'a [ScopeEntry], current: &str, forward: bool) -> Option<&'a ScopeEntry> {
    if entries.is_empty() {
        return None;
    }
    let count = entries.len();
    let next = match entries.iter().position(|entry| entry.reference == current) {
        Some(index) if forward => (index + 1) % count,
        Some(index) => (index + count - 1) % count,
        None if forward => 0,
        None => count - 1,
    };
    entries.get(next)
}

/// The session's provider settings moved onto a catalog model.
#[must_use]
pub fn config_for(
    base: &ProviderConfig,
    entry: &Model,
    credential: super::credentials::CredentialSource,
) -> ProviderConfig {
    let mut config = base.clone();
    config.provider_id.clone_from(&entry.provider);
    entry
        .protocol()
        .unwrap_or("openai_chat")
        .clone_into(&mut config.protocol);
    config.endpoint = entry.endpoint();
    config.model.clone_from(&entry.id);
    config.api_key_env = entry.key_env();
    config.thinking_format = entry
        .compat
        .as_ref()
        .and_then(|compat| compat.thinking_format.clone());
    config.credential = credential;
    config.model_price = entry
        .cost
        .filter(|cost| cost.input > 0.0 || cost.output > 0.0)
        .map(|cost| super::cost::ModelPrice {
            input_per_mtok: cost.input,
            output_per_mtok: cost.output,
        });
    if let Some(window) = entry.context_window {
        config.context_window_tokens = window;
    }
    config
}

/// Why a named model cannot be used.
#[derive(Debug, Eq, PartialEq)]
pub enum Unusable {
    NotInCatalog,
    NoCredential(String),
}

/// A catalog model the session can call: in the catalog and with a key.
pub fn resolve(
    base: &ProviderConfig,
    reference: &str,
    environment: &LaunchEnvironment,
    data_dir: &Path,
) -> Result<(ProviderConfig, Model), Unusable> {
    resolve_scoped(
        base,
        reference,
        environment,
        data_dir,
        super::credentials::Scope::Main,
    )
}

/// [`resolve`] with the key a scope reads: a delegated child's own login first.
pub fn resolve_scoped(
    base: &ProviderConfig,
    reference: &str,
    environment: &LaunchEnvironment,
    data_dir: &Path,
    scope: super::credentials::Scope,
) -> Result<(ProviderConfig, Model), Unusable> {
    let catalog = Catalog::load(data_dir);
    let entry = catalog
        .find(reference)
        .cloned()
        .ok_or(Unusable::NotInCatalog)?;
    let credential = super::credentials::source_for_scope(
        environment,
        data_dir,
        &entry.provider,
        &entry.key_env(),
        scope,
    )
    .ok_or_else(|| Unusable::NoCredential(entry.provider.clone()))?;
    Ok((config_for(base, &entry, credential), entry))
}

/// How long to wait for a rate-limited provider, as prime-agent's provider
/// retry waits: from `base`, doubling, at most `max_delay` a time, at most
/// `max_attempts` checks and `max_total` in all.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UsageWait {
    pub base: Duration,
    pub max_delay: Duration,
    pub max_attempts: u32,
    pub max_total: Duration,
}

impl UsageWait {
    /// prime-agent's `baseDelayMs 1000, maxDelayMs 300000, maxAttempts 30,
    /// maxWaitMs 900000`.
    pub const PRIME: Self = Self {
        base: Duration::from_secs(1),
        max_delay: Duration::from_mins(5),
        max_attempts: 30,
        max_total: Duration::from_mins(15),
    };

    /// The wait before check `attempt` (1-based): the provider's `Retry-After`
    /// when it gave one within the bound, else the doubling backoff.
    #[must_use]
    pub fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        if let Some(wait) = retry_after.filter(|wait| *wait <= self.max_delay) {
            return wait;
        }
        let factor = 2u32.saturating_pow(attempt.saturating_sub(1).min(20));
        self.base.saturating_mul(factor).min(self.max_delay)
    }
}

/// prime-agent's `PROVIDER_RESUME_GRACE_MS`: a park wakes a little after the
/// reported reset, so the window has really rolled over.
pub const RESUME_GRACE: Duration = Duration::from_secs(30);

/// prime-agent's default `maxPauseMs`: the longest one park lasts.
pub const MAX_PAUSE: Duration = Duration::from_hours(24);

/// prime-agent's `QUOTA_RESUME_MARKER_TEXT`: the prompt a parked session wakes with.
pub const QUOTA_RESUME_PROMPT: &str = "<provider_quota_resumed>\nThe provider usage limit that paused this session has been reported as reset; this resume is automatic. Continue the interrupted task from where it stopped.\n</provider_quota_resumed>";

/// prime-agent's `parseProviderResetMs`: a recovery window the provider names
/// in its error text, such as the `ChatGPT` plan's "Try again in ~7272 min."
#[must_use]
pub fn parse_reset(text: &str) -> Option<Duration> {
    let lowered = text.to_ascii_lowercase();
    for anchor in ["try again", "reset", "available"] {
        let mut from = 0;
        while let Some(found) = lowered[from..].find(anchor) {
            let start = from + found + anchor.len();
            from = start;
            // Within 80 characters, before a sentence ends: an optional `~`,
            // a number and a unit.
            let window = lowered[start..]
                .char_indices()
                .take_while(|(index, character)| *index < 80 && *character != '.')
                .map(|(_, character)| character)
                .collect::<String>();
            let Some(digits_at) = window.find(|character: char| character.is_ascii_digit()) else {
                continue;
            };
            let rest = &window[digits_at..];
            let digits = rest
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>();
            let unit = rest[digits.len()..]
                .trim_start()
                .chars()
                .take_while(char::is_ascii_alphabetic)
                .collect::<String>();
            let seconds = match unit.trim_end_matches('s') {
                "second" | "sec" => 1,
                "minute" | "min" => 60,
                "hour" | "hr" => 3_600,
                "day" => 86_400,
                _ => continue,
            };
            let amount = digits.parse::<u64>().ok()?;
            return Some(Duration::from_secs(amount.saturating_mul(seconds)));
        }
    }
    None
}

/// The provider's reported recovery: its `Retry-After`, else a window named in
/// its error text.
fn reported_reset(error: &ProviderError) -> Option<Duration> {
    error
        .retry_after()
        .or_else(|| parse_reset(&error.to_string()))
}

/// The line shown while waiting, prime-agent's.
#[must_use]
pub fn waiting_line(attempt: u32, max_attempts: u32, wait: Duration) -> String {
    format!(
        "Waiting for provider usage to recover ({attempt}/{max_attempts}), next check in {}s... (esc to cancel)",
        wait.as_secs().max(1)
    )
}

/// A model a turn can hand calls to, or why the one configured cannot be used.
pub type Handoff = Result<(String, Arc<dyn ModelProvider>), String>;

/// Where one turn's model calls go when the session model cannot take them:
/// the backup model after the session model keeps failing, the image model for
/// a request with images the session model cannot read, and a wait for a
/// rate-limited provider to recover.
pub struct Router {
    sender: UnboundedSender<SessionEvent>,
    primary: String,
    primary_takes_images: bool,
    backup: Option<Handoff>,
    image: Option<Handoff>,
    wait: Option<UsageWait>,
    /// The runtime's attempts per call: a transient failure this many times in
    /// a row moves the turn to the backup model.
    max_attempts: u32,
    /// Session-wide: the primary a previous turn left for the backup, so its
    /// recovery is announced once.
    left_primary: Arc<Mutex<Option<String>>>,
    failures: AtomicU32,
    on_backup: AtomicBool,
    gave_up: AtomicBool,
    told: Mutex<Vec<String>>,
}

impl Router {
    #[must_use]
    #[allow(
        clippy::too_many_arguments,
        reason = "each routing role is its own input"
    )]
    pub fn new(
        sender: UnboundedSender<SessionEvent>,
        primary: String,
        primary_takes_images: bool,
        backup: Option<Handoff>,
        image: Option<Handoff>,
        wait: Option<UsageWait>,
        max_attempts: u32,
        left_primary: Arc<Mutex<Option<String>>>,
    ) -> Self {
        Self {
            sender,
            primary,
            primary_takes_images,
            backup,
            image,
            wait,
            max_attempts: max_attempts.max(1),
            left_primary,
            failures: AtomicU32::new(0),
            on_backup: AtomicBool::new(false),
            gave_up: AtomicBool::new(false),
            told: Mutex::new(Vec::new()),
        }
    }

    fn notice(&self, message: String) {
        let _ = self.sender.send(SessionEvent::Notice { message });
    }

    fn waiting(&self, line: Option<String>) {
        let _ = self.sender.send(SessionEvent::ProviderWaiting { line });
    }

    /// A notice said at most once a turn.
    fn notice_once(&self, message: String) {
        let Ok(mut told) = self.told.lock() else {
            return;
        };
        if !told.contains(&message) {
            told.push(message.clone());
            self.notice(message);
        }
    }

    /// One model call, routed.
    pub fn stream_events(
        self: &Arc<Self>,
        primary: Arc<dyn ModelProvider>,
        request: ProviderRequest,
        cancellation: CancellationToken,
    ) -> ProviderEventStream {
        let has_images = request
            .messages
            .iter()
            .any(|message| !message.attachments.is_empty());
        if has_images && !self.primary_takes_images {
            return match &self.image {
                Some(Ok((name, provider))) => {
                    self.notice_once(format!(
                        "{} does not accept images; this request goes to the image model {name}",
                        self.primary
                    ));
                    provider.stream_events(request, cancellation)
                }
                Some(Err(reason)) => failed(ProviderError::new(
                    ErrorCode::IncompatibleService,
                    format!(
                        "This model does not accept images, and the image model is unusable: {reason}"
                    ),
                )),
                None => failed(ProviderError::new(
                    ErrorCode::IncompatibleService,
                    "This model does not accept images; set [routing] image in config",
                )),
            };
        }
        if self.on_backup.load(Ordering::SeqCst)
            && let Some(Ok((_, backup))) = &self.backup
        {
            return backup.stream_events(request, cancellation);
        }
        let router = Arc::clone(self);
        Box::pin(
            futures_util::stream::once(
                async move { router.open(primary, request, cancellation).await },
            )
            .flatten(),
        )
    }

    async fn open(
        self: Arc<Self>,
        primary: Arc<dyn ModelProvider>,
        request: ProviderRequest,
        cancellation: CancellationToken,
    ) -> ProviderEventStream {
        let mut pings = 0u32;
        let mut waited = Duration::ZERO;
        loop {
            let mut stream = primary.stream_events(request.clone(), cancellation.clone());
            // A refusal comes before any output; `Started` is not output.
            let mut head = Vec::new();
            let first = loop {
                match stream.next().await {
                    Some(Ok(event @ ProviderStreamEvent::Started { .. })) => head.push(Ok(event)),
                    other => break other,
                }
            };
            let error = match first {
                Some(Err(error)) if is_transient(&error) => error,
                first => {
                    if pings > 0 {
                        self.waiting(None);
                    }
                    if matches!(first, Some(Ok(_))) {
                        self.failures.store(0, Ordering::SeqCst);
                        let left = self
                            .left_primary
                            .lock()
                            .ok()
                            .and_then(|mut left| left.take());
                        if left.is_some() {
                            self.notice(format!(
                                "Primary provider recovered — back on {}",
                                self.primary
                            ));
                        }
                    }
                    return Box::pin(
                        futures_util::stream::iter(head.into_iter().chain(first)).chain(stream),
                    );
                }
            };
            let failures = self.failures.fetch_add(1, Ordering::SeqCst) + 1;
            let rate_limited = error.code() == ErrorCode::RateLimited;
            // The backup takes over once the session model has used up its
            // retries; a rate limit is not waited out while a backup can answer.
            if rate_limited || failures >= self.max_attempts {
                match &self.backup {
                    Some(Ok((name, backup))) => {
                        if pings > 0 {
                            self.waiting(None);
                        }
                        self.on_backup.store(true, Ordering::SeqCst);
                        if let Ok(mut left) = self.left_primary.lock() {
                            *left = Some(self.primary.clone());
                        }
                        self.notice(format!(
                            "Primary model unavailable ({error}) — retrying on backup model {name}..."
                        ));
                        return backup.stream_events(request, cancellation);
                    }
                    Some(Err(reason)) => {
                        self.notice_once(format!("backup model unusable: {reason}"));
                    }
                    None => {}
                }
            }
            if rate_limited && self.gave_up.load(Ordering::SeqCst) {
                // The wait already gave up or parked this turn: the runtime's own
                // retries end at once instead of sleeping on the provider's reset.
                return failed(error.with_retry_after(None));
            }
            let Some(wait) = self.wait.filter(|_| rate_limited) else {
                return Box::pin(futures_util::stream::iter(
                    head.into_iter().chain([Err(error)]),
                ));
            };
            pings += 1;
            let delay = match self.next_delay(&error, wait, waited, pings) {
                Ok(delay) => delay,
                Err(parked) => return failed(parked),
            };
            if pings > wait.max_attempts || waited + delay > wait.max_total {
                self.gave_up.store(true, Ordering::SeqCst);
                self.waiting(None);
                return failed(ProviderError::new(
                    ErrorCode::RateLimited,
                    format!(
                        "Provider recovery wait gave up after {} pings: {error}",
                        pings - 1
                    ),
                ));
            }
            self.waiting(Some(waiting_line(pings, wait.max_attempts, delay)));
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = cancellation.cancelled() => {
                    self.waiting(None);
                    return failed(ProviderError::new(
                        ErrorCode::ProviderCanceled,
                        "provider usage wait canceled",
                    ));
                }
            }
            waited += delay;
        }
    }
}

impl Router {
    /// prime-agent's `providerWaitDecision`: a reported reset within the
    /// remaining bound is waited for exactly; one beyond it parks the session
    /// until then (the error is the turn's); otherwise the doubling backoff.
    fn next_delay(
        &self,
        error: &ProviderError,
        wait: UsageWait,
        waited: Duration,
        pings: u32,
    ) -> Result<Duration, ProviderError> {
        match reported_reset(error) {
            Some(reset) if reset > wait.max_total.saturating_sub(waited) => {
                Err(self.park(reset, pings - 1, wait, error))
            }
            Some(reset) => Ok(reset),
            None => Ok(wait.delay(pings, None)),
        }
    }

    /// prime-agent's `_parkForQuotaReset`: the session waits for the reported
    /// reset (plus a grace, at most a day) as a durable one-shot job instead of
    /// polling, and the turn ends saying so.
    fn park(
        &self,
        reset: Duration,
        attempts: u32,
        wait: UsageWait,
        error: &ProviderError,
    ) -> ProviderError {
        self.waiting(None);
        self.gave_up.store(true, Ordering::SeqCst);
        let pause = (reset + RESUME_GRACE).min(MAX_PAUSE);
        let until = chrono::Utc::now()
            + chrono::Duration::from_std(pause).unwrap_or(chrono::Duration::zero());
        let at = until.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let _ = self.sender.send(SessionEvent::QuotaParked { until });
        ProviderError::new(
            ErrorCode::RateLimited,
            format!(
                "Retry failed after {attempts} attempts: provider reported recovery in {}s, beyond the wait bound of {}s. Session parked until {at} and will resume automatically: {error}",
                reset.as_secs(),
                wait.max_total.as_secs()
            ),
        )
    }
}

/// A failure worth routing around: a rate limit or an unavailable service.
fn is_transient(error: &ProviderError) -> bool {
    matches!(
        error.code(),
        ErrorCode::RateLimited | ErrorCode::ServiceUnavailable
    )
}

fn failed(error: ProviderError) -> ProviderEventStream {
    Box::pin(futures_util::stream::iter([Err(error)]))
}

/// The buffered view of a routed call.
pub fn collect(stream: ProviderEventStream) -> ProviderFuture {
    Box::pin(async move {
        let mut stream = stream;
        let mut events = Vec::new();
        while let Some(item) = stream.next().await {
            events.push(item?);
        }
        Ok(events)
    })
}

/// Summaries written by the auxiliary model, as prime-agent's
/// `_resolveAuxiliaryModel` hands compaction to it: when it cannot write one,
/// the session model does, and the user is told once.
pub struct AuxiliarySummary {
    name: String,
    auxiliary: Arc<dyn harness_runtime::SummaryProvider>,
    session: Arc<dyn harness_runtime::SummaryProvider>,
    sender: UnboundedSender<SessionEvent>,
    told: AtomicBool,
}

impl AuxiliarySummary {
    #[must_use]
    pub fn new(
        name: String,
        auxiliary: Arc<dyn harness_runtime::SummaryProvider>,
        session: Arc<dyn harness_runtime::SummaryProvider>,
        sender: UnboundedSender<SessionEvent>,
    ) -> Self {
        Self {
            name,
            auxiliary,
            session,
            sender,
            told: AtomicBool::new(false),
        }
    }

    fn either(
        &self,
        call: impl Fn(
            &dyn harness_runtime::SummaryProvider,
        ) -> Result<String, harness_runtime::RuntimeError>,
    ) -> Result<String, harness_runtime::RuntimeError> {
        match call(self.auxiliary.as_ref()) {
            Ok(summary) => Ok(summary),
            Err(error) => {
                if !self.told.swap(true, Ordering::SeqCst) {
                    let _ = self.sender.send(SessionEvent::Notice {
                        message: unusable_auxiliary(&self.name, "compaction", &error.to_string()),
                    });
                }
                call(self.session.as_ref())
            }
        }
    }
}

/// prime-agent's notice for an auxiliary model that cannot do its job.
#[must_use]
pub fn unusable_auxiliary(name: &str, purpose: &str, reason: &str) -> String {
    format!("auxiliaryModel \"{name}\" unusable for {purpose} ({reason}); using the session model.")
}

impl harness_runtime::SummaryProvider for AuxiliarySummary {
    fn summarize(
        &self,
        recovery: &harness_session::RecoveryView,
    ) -> Result<String, harness_runtime::RuntimeError> {
        self.either(|provider| provider.summarize(recovery))
    }

    fn summarize_conversation(
        &self,
        prompt: &str,
        budget_tokens: u64,
    ) -> Result<String, harness_runtime::RuntimeError> {
        self.either(|provider| provider.summarize_conversation(prompt, budget_tokens))
    }

    fn summarize_bounded(
        &self,
        recovery: &harness_session::RecoveryView,
        budget_tokens: u64,
    ) -> Result<String, harness_runtime::RuntimeError> {
        self.either(|provider| provider.summarize_bounded(recovery, budget_tokens))
    }

    fn summarize_with_guidance(
        &self,
        recovery: &harness_session::RecoveryView,
        budget_tokens: u64,
        guidance: Option<&str>,
    ) -> Result<String, harness_runtime::RuntimeError> {
        self.either(|provider| provider.summarize_with_guidance(recovery, budget_tokens, guidance))
    }
}

#[cfg(test)]
mod tests {
    use super::{ScopeEntry, UsageWait, glob, scope, step};
    use crate::interactive::providers::Catalog;
    use harness_providers::ThinkingLevel;
    use std::time::Duration;

    #[test]
    fn q11_scope_patterns_match_like_prime() {
        assert!(glob("deepseek/*", "deepseek/deepseek-v4-flash"));
        assert!(glob("GPT-5*", "gpt-5.5"));
        assert!(glob("gpt-5.?", "gpt-5.5"));
        assert!(!glob("gpt-5", "gpt-5.5"));
        let models = Catalog::bundled().models().to_vec();
        let entries = scope(
            &["deepseek/*".to_owned(), "openai/gpt-5*:high".to_owned()],
            &models,
        );
        assert!(!entries.is_empty());
        assert!(entries[0].reference.starts_with("deepseek/"));
        let openai = entries
            .iter()
            .find(|entry| entry.reference.starts_with("openai/"))
            .expect("an openai model is in scope");
        assert_eq!(openai.level, Some(ThinkingLevel::parse("high").unwrap()));
        let references = entries
            .iter()
            .map(|entry| &entry.reference)
            .collect::<Vec<_>>();
        let mut unique = references.clone();
        unique.dedup();
        assert_eq!(references.len(), unique.len(), "each model once");
        assert!(scope(&["nothing/*".to_owned()], &models).is_empty());
    }

    #[test]
    fn q11_step_wraps_around_both_ways() {
        let entries = ["a/1", "b/2", "c/3"]
            .map(|reference| ScopeEntry {
                reference: reference.to_owned(),
                level: None,
            })
            .to_vec();
        assert_eq!(step(&entries, "a/1", true).unwrap().reference, "b/2");
        assert_eq!(step(&entries, "c/3", true).unwrap().reference, "a/1");
        assert_eq!(step(&entries, "a/1", false).unwrap().reference, "c/3");
        assert_eq!(step(&entries, "x/9", true).unwrap().reference, "a/1");
        assert!(step(&[], "a/1", true).is_none());
    }

    /// prime-agent's `parseProviderResetMs`: the window a provider names in its
    /// error text.
    #[test]
    fn a_reset_named_in_the_error_text_is_read() {
        use super::parse_reset;
        use std::time::Duration;
        assert_eq!(
            parse_reset(
                "You have hit your ChatGPT usage limit (plus plan). Try again in ~7272 min."
            ),
            Some(Duration::from_mins(7272))
        );
        assert_eq!(
            parse_reset("quota exceeded; resets in 2 hours"),
            Some(Duration::from_hours(2))
        );
        assert_eq!(parse_reset("rate limited. Try later"), None);
        assert_eq!(parse_reset("provider returned HTTP 429"), None);
    }

    #[test]
    fn q13_backoff_doubles_to_the_cap_and_honours_retry_after() {
        let wait = UsageWait::PRIME;
        assert_eq!(wait.delay(1, None), Duration::from_secs(1));
        assert_eq!(wait.delay(4, None), Duration::from_secs(8));
        assert_eq!(wait.delay(20, None), Duration::from_mins(5));
        assert_eq!(
            wait.delay(3, Some(Duration::from_secs(2))),
            Duration::from_secs(2)
        );
        assert_eq!(
            wait.delay(1, Some(Duration::from_mins(10))),
            Duration::from_secs(1)
        );
    }

    /// A provider that fails with the scripted errors, then answers.
    struct Failing {
        errors: std::sync::Mutex<std::collections::VecDeque<harness_providers::ProviderError>>,
        forever: Option<harness_providers::ProviderError>,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl Failing {
        fn new(codes: &[harness_types::ErrorCode]) -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self {
                errors: std::sync::Mutex::new(
                    codes
                        .iter()
                        .map(|code| harness_providers::ProviderError::new(*code, "scripted"))
                        .collect(),
                ),
                forever: None,
                calls: std::sync::atomic::AtomicUsize::new(0),
            })
        }

        fn always(code: harness_types::ErrorCode) -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self {
                errors: std::sync::Mutex::new(std::collections::VecDeque::new()),
                forever: Some(harness_providers::ProviderError::new(code, "scripted")),
                calls: std::sync::atomic::AtomicUsize::new(0),
            })
        }

        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl harness_providers::ModelProvider for Failing {
        fn capabilities(&self) -> harness_providers::ModelCapabilities {
            harness_providers::ModelCapabilities::deepseek_fixture()
        }

        fn stream(
            &self,
            request: harness_providers::ProviderRequest,
            cancellation: harness_providers::CancellationToken,
        ) -> harness_providers::ProviderFuture {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let next = self
                .errors
                .lock()
                .unwrap()
                .pop_front()
                .or_else(|| self.forever.clone());
            match next {
                Some(error) => Box::pin(async move { Err(error) }),
                None => harness_providers::MockProvider::text("primary-answer")
                    .stream(request, cancellation),
            }
        }
    }

    type Events = tokio::sync::mpsc::UnboundedReceiver<crate::interactive::events::SessionEvent>;

    fn router(
        primary_takes_images: bool,
        backup: Option<super::Handoff>,
        image: Option<super::Handoff>,
        wait: Option<UsageWait>,
        left: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    ) -> (std::sync::Arc<super::Router>, Events) {
        let (sender, events) = tokio::sync::mpsc::unbounded_channel();
        (
            std::sync::Arc::new(super::Router::new(
                sender,
                "main/primary".to_owned(),
                primary_takes_images,
                backup,
                image,
                wait,
                3,
                left,
            )),
            events,
        )
    }

    #[allow(clippy::unnecessary_wraps, reason = "a handoff is a Result")]
    fn answering(text: &str) -> super::Handoff {
        Ok((
            format!("other/{text}"),
            std::sync::Arc::new(harness_providers::MockProvider::text(text)),
        ))
    }

    fn request(image: bool) -> harness_providers::ProviderRequest {
        let mut message =
            harness_providers::ProviderMessage::new(harness_providers::MessageRole::User, "look");
        if image {
            message = message.with_attachments(vec![harness_providers::ImageAttachment::inline(
                "image/png",
                "AAAA",
                "shot.png",
            )]);
        }
        harness_providers::ProviderRequest::new(
            harness_types::RequestId::generate(),
            "primary",
            vec![message],
        )
    }

    async fn call(
        router: &std::sync::Arc<super::Router>,
        primary: std::sync::Arc<dyn harness_providers::ModelProvider>,
        request: harness_providers::ProviderRequest,
        cancellation: harness_providers::CancellationToken,
    ) -> Result<String, harness_providers::ProviderError> {
        let events = super::collect(router.stream_events(primary, request, cancellation)).await?;
        Ok(harness_providers::assemble_stream(&events)
            .expect("a complete answer")
            .text)
    }

    fn said(events: &mut Events) -> Vec<String> {
        let mut said = Vec::new();
        while let Ok(event) = events.try_recv() {
            match event {
                crate::interactive::events::SessionEvent::Notice { message } => said.push(message),
                crate::interactive::events::SessionEvent::ProviderWaiting { line } => {
                    said.push(line.unwrap_or_else(|| "<wait over>".to_owned()));
                }
                _ => {}
            }
        }
        said
    }

    fn quick(max_attempts: u32) -> UsageWait {
        UsageWait {
            base: Duration::from_millis(1),
            max_delay: Duration::from_millis(4),
            max_attempts,
            max_total: Duration::from_secs(5),
        }
    }

    #[tokio::test]
    async fn q12_backup_takes_over_after_retries_and_hands_back() {
        use harness_types::ErrorCode::ServiceUnavailable;
        let left = std::sync::Arc::new(std::sync::Mutex::new(None));
        let primary = Failing::new(&[ServiceUnavailable, ServiceUnavailable, ServiceUnavailable]);
        let (turn, mut events) = router(
            true,
            Some(answering("backup")),
            None,
            None,
            std::sync::Arc::clone(&left),
        );
        let token = harness_providers::CancellationToken::new;
        // The runtime's first two attempts fail as they would without a backup.
        for _ in 0..2 {
            let error = call(&turn, primary.clone(), request(false), token())
                .await
                .unwrap_err();
            assert_eq!(error.code(), ServiceUnavailable);
        }
        // Its last attempt is answered by the backup, and so is the rest of the turn.
        assert_eq!(
            call(&turn, primary.clone(), request(false), token())
                .await
                .unwrap(),
            "backup"
        );
        assert_eq!(
            call(&turn, primary.clone(), request(false), token())
                .await
                .unwrap(),
            "backup"
        );
        assert_eq!(
            primary.calls(),
            3,
            "the primary is not called again this turn"
        );
        let notices = said(&mut events);
        assert!(
            notices
                .iter()
                .any(|notice| notice.starts_with("Primary model unavailable (")
                    && notice.ends_with("retrying on backup model other/backup...")),
            "{notices:?}"
        );
        // The next turn is back on the primary, and says so once.
        let (next, mut events) = router(true, Some(answering("backup")), None, None, left);
        assert_eq!(
            call(&next, primary.clone(), request(false), token())
                .await
                .unwrap(),
            "primary-answer"
        );
        assert_eq!(
            call(&next, primary.clone(), request(false), token())
                .await
                .unwrap(),
            "primary-answer"
        );
        let notices = said(&mut events);
        assert_eq!(
            notices,
            ["Primary provider recovered — back on main/primary"]
        );
    }

    #[tokio::test]
    async fn q12_images_route_to_the_image_model() {
        let primary = Failing::new(&[]);
        let (turn, mut events) = router(
            false,
            None,
            Some(answering("vision")),
            None,
            std::sync::Arc::default(),
        );
        let token = harness_providers::CancellationToken::new;
        assert_eq!(
            call(&turn, primary.clone(), request(true), token())
                .await
                .unwrap(),
            "vision"
        );
        assert_eq!(
            call(&turn, primary.clone(), request(false), token())
                .await
                .unwrap(),
            "primary-answer"
        );
        assert_eq!(
            primary.calls(),
            1,
            "only the text request reached the primary"
        );
        assert!(said(&mut events)[0].contains("image model other/vision"));
        // A model that reads images keeps them.
        let (reads, _events) = router(
            true,
            None,
            Some(answering("vision")),
            None,
            std::sync::Arc::default(),
        );
        assert_eq!(
            call(&reads, primary.clone(), request(true), token())
                .await
                .unwrap(),
            "primary-answer"
        );
    }

    #[tokio::test]
    async fn q12_images_without_an_image_model_fail_clearly() {
        let primary = Failing::new(&[]);
        let (turn, _events) = router(false, None, None, None, std::sync::Arc::default());
        let error = call(
            &turn,
            primary.clone(),
            request(true),
            harness_providers::CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), harness_types::ErrorCode::IncompatibleService);
        assert!(error.to_string().contains("set [routing] image"), "{error}");
        assert_eq!(primary.calls(), 0, "the image never reached a text model");
    }

    #[tokio::test]
    async fn q13_rate_limit_waits_and_recovers() {
        use harness_types::ErrorCode::RateLimited;
        let primary = Failing::new(&[RateLimited, RateLimited]);
        let (turn, mut events) =
            router(true, None, None, Some(quick(30)), std::sync::Arc::default());
        let answer = call(
            &turn,
            primary.clone(),
            request(false),
            harness_providers::CancellationToken::new(),
        )
        .await;
        assert_eq!(answer.unwrap(), "primary-answer");
        assert_eq!(primary.calls(), 3);
        let lines = said(&mut events);
        assert!(
            lines[0].starts_with("Waiting for provider usage to recover (1/30), next check in 1s"),
            "{lines:?}"
        );
        assert!(lines[0].ends_with("(esc to cancel)"));
        assert!(lines[1].contains("(2/30)"));
        assert_eq!(lines.last().map(String::as_str), Some("<wait over>"));
    }

    #[tokio::test]
    async fn q13_rate_limit_wait_is_cancelable() {
        let primary = Failing::always(harness_types::ErrorCode::RateLimited);
        let wait = UsageWait {
            base: Duration::from_mins(1),
            ..UsageWait::PRIME
        };
        let (turn, _events) = router(true, None, None, Some(wait), std::sync::Arc::default());
        let cancellation = harness_providers::CancellationToken::new();
        let cancel = cancellation.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel.cancel();
        });
        let started = std::time::Instant::now();
        let error = call(&turn, primary.clone(), request(false), cancellation)
            .await
            .unwrap_err();
        assert_eq!(error.code(), harness_types::ErrorCode::ProviderCanceled);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn q13_wait_gives_up_after_the_bound() {
        let primary = Failing::always(harness_types::ErrorCode::RateLimited);
        let (turn, _events) = router(true, None, None, Some(quick(2)), std::sync::Arc::default());
        let token = harness_providers::CancellationToken::new;
        let error = call(&turn, primary.clone(), request(false), token())
            .await
            .unwrap_err();
        assert_eq!(error.code(), harness_types::ErrorCode::RateLimited);
        assert!(
            error.to_string().contains("gave up after 2 pings"),
            "{error}"
        );
        assert_eq!(primary.calls(), 3);
        // The runtime's retry of the same call does not start a second wait.
        call(&turn, primary.clone(), request(false), token())
            .await
            .unwrap_err();
        assert_eq!(primary.calls(), 4);
    }

    #[tokio::test]
    async fn q13_a_rate_limit_goes_to_the_backup_before_any_wait() {
        let primary = Failing::always(harness_types::ErrorCode::RateLimited);
        let (turn, _events) = router(
            true,
            Some(answering("backup")),
            None,
            Some(quick(30)),
            std::sync::Arc::default(),
        );
        let answer = call(
            &turn,
            primary.clone(),
            request(false),
            harness_providers::CancellationToken::new(),
        )
        .await;
        assert_eq!(answer.unwrap(), "backup");
        assert_eq!(primary.calls(), 1);
    }

    struct Fixed(Result<&'static str, &'static str>);

    impl harness_runtime::SummaryProvider for Fixed {
        fn summarize(
            &self,
            _recovery: &harness_session::RecoveryView,
        ) -> Result<String, harness_runtime::RuntimeError> {
            unreachable!("compaction summarises the conversation")
        }

        fn summarize_conversation(
            &self,
            _prompt: &str,
            _budget_tokens: u64,
        ) -> Result<String, harness_runtime::RuntimeError> {
            self.0.map(str::to_owned).map_err(|reason| {
                harness_runtime::RuntimeError::new(
                    harness_types::ErrorCode::ServiceUnavailable,
                    reason,
                )
            })
        }
    }

    #[test]
    fn q12_auxiliary_model_writes_the_summary() {
        use harness_runtime::SummaryProvider;
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        let summary = super::AuxiliarySummary::new(
            "cheap/model".to_owned(),
            std::sync::Arc::new(Fixed(Ok("from-auxiliary"))),
            std::sync::Arc::new(Fixed(Ok("from-session"))),
            sender,
        );
        assert_eq!(
            summary.summarize_conversation("history", 100).unwrap(),
            "from-auxiliary"
        );
        assert!(said(&mut events).is_empty());
    }

    #[test]
    fn q12_unusable_auxiliary_falls_back_with_notice() {
        use harness_runtime::SummaryProvider;
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        let summary = super::AuxiliarySummary::new(
            "cheap/model".to_owned(),
            std::sync::Arc::new(Fixed(Err("context too small"))),
            std::sync::Arc::new(Fixed(Ok("from-session"))),
            sender,
        );
        assert_eq!(
            summary.summarize_conversation("history", 100).unwrap(),
            "from-session"
        );
        assert_eq!(
            summary.summarize_conversation("history", 100).unwrap(),
            "from-session"
        );
        let notices = said(&mut events);
        assert_eq!(notices.len(), 1, "told once: {notices:?}");
        assert!(notices[0].starts_with("auxiliaryModel \"cheap/model\" unusable for compaction ("));
        assert!(notices[0].ends_with("; using the session model."));
    }
}
