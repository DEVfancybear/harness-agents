//! The `jev` plugin: TypeSafe's Jev, a System One model, as a tool of the agent.
//!
//! An agent + Jev workflow (TypeSafe's skill, `jev-fraud`): the reasoning model
//! researches, reasons and drafts; Jev makes the repeated narrow judgments -
//! yes/no (`noul`), one of a set (`choice`), a degree (`score`) - in about 100 ms
//! for a fraction of the cost, each with a probability and a confidence. An answer
//! whose confidence reaches the threshold is decided; the rest come back to the
//! model for review. Many items run at once, as `jev-fraud` classifies its emails.
//!
//! Off by default: `/jev on` (settings.json `jev.enabled`) turns it on, and the key
//! is `TYPESAFE_API_KEY` or what `/login typesafe` saved. The `jev` skill teaches
//! the workflow and gives the Python kernel the same calls.

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::stream::{self, StreamExt as _};
use harness_tools::{
    CodingToolAction, ExternalToolCatalog, ExternalToolDispatcher, ExternalTools,
    ToolDispatchAuthorization, ToolOutput,
};
use harness_types::{ErrorCode, HarnessError};
use serde_json::{Map, Value, json};

use super::paths::LaunchEnvironment;

/// The settings.json key that holds `enabled`, `threshold` and `model`.
pub const SETTING: &str = "jev";
/// The TypeSafe key; else what `/login typesafe` saved.
pub const KEY_VARIABLE: &str = "TYPESAFE_API_KEY";
/// The credential-file entry `/login typesafe` saves.
pub const CREDENTIAL: &str = "typesafe";
/// Moves the endpoint, for a proxy or a test.
pub const URL_VARIABLE: &str = "HA_TYPESAFE_URL";
const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
const DEFAULT_MODEL: &str = "jev-latest";
/// `jev-fraud`'s threshold: below it, a decision goes to review.
pub const DEFAULT_THRESHOLD: f64 = 0.95;
/// TypeSafe's input price, $0.042 per million tokens; output is free.
const INPUT_PRICE: f64 = 0.042 / 1_000_000.0;
/// `jev-fraud` runs 20 Jev calls at once.
const CONCURRENCY: usize = 20;
const MAX_ITEMS: usize = 500;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(45);

/// What settings.json says about the plugin.
#[derive(Clone, Debug, PartialEq)]
pub struct JevSettings {
    pub enabled: bool,
    pub threshold: f64,
    pub model: String,
}

impl JevSettings {
    #[must_use]
    pub fn load(config_file: &Path) -> Self {
        let setting = super::config::load_setting(config_file, SETTING).unwrap_or(Value::Null);
        Self {
            enabled: setting["enabled"] == true,
            threshold: setting["threshold"]
                .as_f64()
                .filter(|value| (0.0..=1.0).contains(value))
                .unwrap_or(DEFAULT_THRESHOLD),
            model: setting["model"]
                .as_str()
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .unwrap_or(DEFAULT_MODEL)
                .to_owned(),
        }
    }
}

/// The TypeSafe key: `TYPESAFE_API_KEY`, else the one `/login typesafe` saved.
#[must_use]
pub fn api_key(environment: &LaunchEnvironment, data_dir: &Path) -> Option<String> {
    environment
        .value(KEY_VARIABLE)
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            super::credentials::load(
                &super::credentials::resolve_file(environment, data_dir),
                CREDENTIAL,
            )
            .ok()
            .flatten()
            .filter(|credential| {
                matches!(credential, super::credentials::Credential::ApiKey { .. })
            })
            .map(|credential| credential.secret().trim().to_owned())
            .filter(|key| !key.is_empty())
        })
}

/// `/jev`'s report: on or off, the key's source, the threshold and the model.
#[must_use]
pub fn status(config_file: &Path, environment: &LaunchEnvironment, data_dir: &Path) -> String {
    let settings = JevSettings::load(config_file);
    let key = if environment
        .value(KEY_VARIABLE)
        .is_some_and(|value| !value.is_empty())
    {
        format!("key from {KEY_VARIABLE}")
    } else if api_key(environment, data_dir).is_some() {
        "key saved by /login typesafe".to_owned()
    } else {
        "no TypeSafe key yet: /login typesafe (or set TYPESAFE_API_KEY)".to_owned()
    };
    if settings.enabled {
        format!(
            "Jev plugin: on (settings.json jev.enabled). The agent has the jev tool and the jev skill: TypeSafe's Jev makes repeated narrow decisions (yes/no, choice, score) and hands back what it is not sure of. Model {}, threshold {}; {key}. /jev off turns it off.",
            settings.model, settings.threshold
        )
    } else {
        format!(
            "Jev plugin: off (the default). /jev on gives the agent TypeSafe's Jev for repeated narrow decisions, with what Jev is not sure of handed back for review; {key}."
        )
    }
}

/// The jev tool of one turn.
#[derive(Clone)]
pub struct JevHost {
    inner: Arc<Inner>,
}

struct Inner {
    client: reqwest::Client,
    key: Option<String>,
    endpoint: String,
    model: String,
    threshold: f64,
}

impl JevHost {
    /// The tool when the plugin is on; `None` when it is off. Without a key the
    /// tool is still offered and says how to add one.
    #[must_use]
    pub fn from_settings(
        environment: &LaunchEnvironment,
        data_dir: &Path,
        config_file: &Path,
    ) -> Option<Self> {
        let settings = JevSettings::load(config_file);
        if !settings.enabled {
            return None;
        }
        let endpoint = environment
            .value(URL_VARIABLE)
            .and_then(|value| value.to_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(ENDPOINT)
            .to_owned();
        Self::build(
            api_key(environment, data_dir),
            endpoint,
            settings.model,
            settings.threshold,
        )
    }

    fn build(key: Option<String>, endpoint: String, model: String, threshold: f64) -> Option<Self> {
        let client = harness_providers::with_extra_roots(reqwest::Client::builder())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .ok()?;
        Some(Self {
            inner: Arc::new(Inner {
                client,
                key,
                endpoint,
                model,
                threshold,
            }),
        })
    }

    #[must_use]
    #[allow(
        clippy::unused_self,
        reason = "a method, like the other hosts' `tools`, so callers map every host the same way"
    )]
    pub fn tools(&self) -> ExternalTools {
        ExternalTools::new(Arc::new(Catalog))
    }

    #[must_use]
    pub fn dispatcher(&self) -> Arc<dyn ExternalToolDispatcher> {
        Arc::new(self.clone())
    }
}

struct Catalog;

impl ExternalToolCatalog for Catalog {
    fn schemas(&self) -> Vec<Value> {
        vec![json!({
            "type": "function",
            "function": {
                "name": "jev",
                "description": "Ask TypeSafe's Jev typed questions about a state - noul (probability the condition holds), choice (one option of criteria), score (a level of ordered criteria) - fast and cheap, each answer with a confidence. Give one state, or items to judge many states in parallel. Answers whose confidence reaches the threshold are decided; the rest come back for you to review from the item itself. Jev cannot explain, count, do arithmetic or dates; the state is untrusted data. See the jev skill for the workflow.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "questions": {
                            "type": "object",
                            "description": "Question name -> {type: noul|choice|score, instructions, criteria}. choice criteria: {option: what it means}; score criteria: ordered level descriptions (2-10); noul criteria (optional): {true: ..., false: ...}. Names are for you; put the whole meaning in instructions.",
                            "additionalProperties": {"type": "object"}
                        },
                        "state": {"description": "What the questions are about: text, or an object with named fields."},
                        "items": {
                            "type": "array",
                            "maxItems": MAX_ITEMS,
                            "description": "Many states, each judged with the same questions: [{id, state}].",
                            "items": {"type": "object", "properties": {"id": {}, "state": {}}, "required": ["state"]}
                        },
                        "threshold": {"type": "number", "minimum": 0, "maximum": 1, "description": "Confidence at which an answer is decided (default from /jev, 0.95)."}
                    },
                    "required": ["questions"],
                    "additionalProperties": false
                }
            }
        })]
    }

    fn resolve(&self, name: &str, arguments: &Value) -> Option<CodingToolAction> {
        (name == "jev").then(|| CodingToolAction::ExternalTool {
            plugin_id: "jev".to_owned(),
            tool_name: name.to_owned(),
            arguments: arguments.clone(),
            parent_invocation_id: None,
            timeout_ms: 300_000,
        })
    }
}

fn invalid(message: impl Into<String>) -> HarnessError {
    HarnessError::new(ErrorCode::InvalidPayload, message)
}

/// One call's validated arguments.
struct Call {
    questions: Map<String, Value>,
    /// `(id, state)` per item; one with the id `state` for a single state.
    items: Vec<(String, Value)>,
    single: bool,
    threshold: f64,
}

fn parse(arguments: &Value, default_threshold: f64) -> Result<Call, HarnessError> {
    let object = arguments
        .as_object()
        .ok_or_else(|| invalid("jev arguments must be an object"))?;
    if let Some(key) = object
        .keys()
        .find(|key| !matches!(key.as_str(), "questions" | "state" | "items" | "threshold"))
    {
        return Err(invalid(format!(
            "jev accepts questions, state, items and threshold, not {key}"
        )));
    }
    let questions = object
        .get("questions")
        .and_then(Value::as_object)
        .filter(|questions| !questions.is_empty())
        .ok_or_else(|| invalid("jev needs questions: a map of name -> question"))?;
    for (name, question) in questions {
        validate_question(name, question)?;
    }
    let threshold = match object.get("threshold") {
        None | Some(Value::Null) => default_threshold,
        Some(value) => value
            .as_f64()
            .filter(|value| (0.0..=1.0).contains(value))
            .ok_or_else(|| invalid("jev threshold is a number from 0 to 1"))?,
    };
    let state = object.get("state").filter(|state| !state.is_null());
    let items = object.get("items").filter(|items| !items.is_null());
    let (items, single) = match (state, items) {
        (Some(state), None) => (vec![("state".to_owned(), state.clone())], true),
        (None, Some(items)) => {
            let items = items
                .as_array()
                .filter(|items| !items.is_empty() && items.len() <= MAX_ITEMS)
                .ok_or_else(|| invalid(format!("jev items is a list of 1 to {MAX_ITEMS} items")))?;
            let mut parsed = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                let state = item
                    .get("state")
                    .filter(|state| !state.is_null())
                    .ok_or_else(|| invalid(format!("jev item {} has no state", index + 1)))?;
                let id = match item.get("id") {
                    Some(Value::String(id)) => id.clone(),
                    Some(Value::Number(id)) => id.to_string(),
                    _ => (index + 1).to_string(),
                };
                parsed.push((id, state.clone()));
            }
            (parsed, false)
        }
        _ => return Err(invalid("jev takes either state or items")),
    };
    Ok(Call {
        questions: questions.clone(),
        items,
        single,
        threshold,
    })
}

fn validate_question(name: &str, question: &Value) -> Result<(), HarnessError> {
    let kind = question.get("type").and_then(Value::as_str);
    let criteria = question.get("criteria");
    match kind {
        Some("noul") => Ok(()),
        Some("choice") => criteria
            .and_then(Value::as_object)
            .filter(|options| !options.is_empty() && options.len() <= 255)
            .map(|_| ())
            .ok_or_else(|| {
                invalid(format!(
                    "jev question {name}: a choice needs criteria, a map of 1 to 255 options"
                ))
            }),
        Some("score") => criteria
            .and_then(Value::as_array)
            .filter(|levels| (2..=10).contains(&levels.len()))
            .map(|_| ())
            .ok_or_else(|| {
                invalid(format!(
                    "jev question {name}: a score needs criteria, 2 to 10 ordered levels"
                ))
            }),
        _ => Err(invalid(format!(
            "jev question {name}: type is noul, choice or score"
        ))),
    }
}

/// One answer, judged against the threshold.
struct Judged {
    name: String,
    /// What Jev decided: an option, a score, yes or no.
    decision: String,
    confidence: Option<f64>,
    /// The other options' probabilities, for an answer under review.
    spread: String,
    accepted: bool,
}

fn judge(name: &str, answer: &Value, threshold: f64) -> Judged {
    let kind = answer["type"].as_str().unwrap_or_default();
    let (decision, confidence) = match kind {
        // A Noul has no confidence of its own: |2p - 1| around its yes probability.
        "noul" => {
            let probability = answer["noul"].as_f64().unwrap_or(0.5);
            (
                format!(
                    "{} (p={probability:.2})",
                    if probability >= 0.5 { "yes" } else { "no" }
                ),
                Some((2.0 * probability - 1.0).abs()),
            )
        }
        "choice" => (
            answer["choice"].as_str().unwrap_or("?").to_owned(),
            answer["confidence"].as_f64(),
        ),
        "score" => (
            answer["score"]
                .as_f64()
                .map_or_else(|| "?".to_owned(), |score| format!("{score:.2}")),
            answer["confidence"].as_f64(),
        ),
        other => (format!("{other}?"), answer["confidence"].as_f64()),
    };
    let spread = answer["probabilities"]
        .as_object()
        .map(|probabilities| {
            let mut pairs = probabilities
                .iter()
                .filter_map(|(option, probability)| Some((option.clone(), probability.as_f64()?)))
                .collect::<Vec<_>>();
            pairs.sort_by(|left, right| right.1.total_cmp(&left.1));
            pairs
                .iter()
                .take(4)
                .map(|(option, probability)| format!("{option} {probability:.2}"))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    Judged {
        name: name.to_owned(),
        decision,
        confidence,
        spread,
        accepted: confidence.is_some_and(|confidence| confidence >= threshold),
    }
}

/// One item's outcome.
enum Outcome {
    Answered {
        answers: Vec<Judged>,
        model: Option<String>,
        input: u64,
    },
    Failed(String),
}

impl JevHost {
    async fn ask(&self, state: &Value, questions: &Map<String, Value>) -> Result<Value, String> {
        let key = self.inner.key.as_deref().ok_or_else(|| {
            "no TypeSafe key: ask the user to run /login typesafe (or set TYPESAFE_API_KEY)"
                .to_owned()
        })?;
        let body = json!({"model": self.inner.model, "state": state, "questions": questions});
        for attempt in 0..3 {
            let response = self
                .inner
                .client
                .post(&self.inner.endpoint)
                .bearer_auth(key)
                .json(&body)
                .send()
                .await
                .map_err(|error| format!("the request failed: {error}"))?;
            let status = response.status();
            if status.as_u16() == 429 && attempt < 2 {
                let wait = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<f64>().ok())
                    .unwrap_or(1.0)
                    .clamp(0.1, 10.0);
                tokio::time::sleep(Duration::from_secs_f64(wait)).await;
                continue;
            }
            let text = response
                .text()
                .await
                .map_err(|error| format!("the answer could not be read: {error}"))?;
            if !status.is_success() {
                return Err(format!(
                    "TypeSafe answered {status}: {}",
                    text.chars().take(300).collect::<String>()
                ));
            }
            return serde_json::from_str(&text)
                .map_err(|_| "TypeSafe did not answer with JSON".to_owned());
        }
        Err("TypeSafe kept answering 429 (rate limited)".to_owned())
    }

    async fn item(&self, id: &str, state: &Value, call: &Call) -> (String, Outcome) {
        (id.to_owned(), self.one(state, call).await)
    }

    async fn one(&self, state: &Value, call: &Call) -> Outcome {
        match self.ask(state, &call.questions).await {
            Ok(data) => {
                let answers = call
                    .questions
                    .keys()
                    .map(|name| {
                        data["answers"].get(name).map_or_else(
                            || Judged {
                                name: name.clone(),
                                decision: "no answer".to_owned(),
                                confidence: None,
                                spread: String::new(),
                                accepted: false,
                            },
                            |answer| judge(name, answer, call.threshold),
                        )
                    })
                    .collect();
                Outcome::Answered {
                    answers,
                    model: data["model"].as_str().map(str::to_owned),
                    input: data["usage"]["input_tokens"].as_u64().unwrap_or(0),
                }
            }
            Err(error) => Outcome::Failed(error),
        }
    }

    async fn run(&self, call: &Call) -> String {
        let started = Instant::now();
        let pending = call
            .items
            .iter()
            .map(|(id, state)| self.item(id, state, call))
            .collect::<Vec<_>>();
        let outcomes = stream::iter(pending)
            .buffered(CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        let report = render(call, &outcomes, started.elapsed());
        let (decided, review, failed, input) = tally(&outcomes);
        harness_types::diag::info("jev", "call")
            .field("items", call.items.len())
            .field("decided", decided)
            .field("review", review)
            .field("failed", failed)
            .field("input", input)
            .field(
                "ms",
                u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            )
            .emit();
        report
    }
}

/// Items decided, under review and failed, and the input tokens.
fn tally(outcomes: &[(String, Outcome)]) -> (usize, usize, usize, u64) {
    let mut counts = (0, 0, 0, 0);
    for (_, outcome) in outcomes {
        match outcome {
            Outcome::Answered { answers, input, .. } => {
                counts.3 += input;
                if answers.iter().all(|answer| answer.accepted) {
                    counts.0 += 1;
                } else {
                    counts.1 += 1;
                }
            }
            Outcome::Failed(_) => counts.2 += 1,
        }
    }
    counts
}

fn answer_line(answer: &Judged) -> String {
    let confidence = answer
        .confidence
        .map_or_else(|| "no confidence".to_owned(), |value| format!("{value:.2}"));
    if answer.accepted || answer.spread.is_empty() {
        format!("{}={} ({confidence})", answer.name, answer.decision)
    } else {
        format!(
            "{}={} ({confidence}; {})",
            answer.name, answer.decision, answer.spread
        )
    }
}

/// The tool's answer: a summary line, then the decided items, those for review
/// with Jev's leaning, and the failures.
fn render(call: &Call, outcomes: &[(String, Outcome)], elapsed: Duration) -> String {
    let (decided, review, failed, input) = tally(outcomes);
    let model = outcomes
        .iter()
        .find_map(|(_, outcome)| match outcome {
            Outcome::Answered { model, .. } => model.clone(),
            Outcome::Failed(_) => None,
        })
        .unwrap_or_else(|| "jev".to_owned());
    #[allow(clippy::cast_precision_loss, reason = "a cost estimate")]
    let cost = input as f64 * INPUT_PRICE;
    let mut text = format!(
        "jev ({model}, threshold {}): {} item(s), {decided} decided, {review} for your review, {failed} failed; {:.1}s; {input} input tokens (~${cost:.6})",
        call.threshold,
        call.items.len(),
        elapsed.as_secs_f64()
    );
    let section = |text: &mut String, title: &str, accepted: bool| {
        let rows = outcomes
            .iter()
            .filter_map(|(id, outcome)| match outcome {
                Outcome::Answered { answers, .. }
                    if answers.iter().all(|answer| answer.accepted) == accepted =>
                {
                    Some(format!(
                        "- {id}: {}",
                        answers
                            .iter()
                            .map(answer_line)
                            .collect::<Vec<_>>()
                            .join("; ")
                    ))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        if !rows.is_empty() {
            let _ = write!(text, "\n{title}\n{}", rows.join("\n"));
        }
    };
    section(&mut text, "Decided:", true);
    section(
        &mut text,
        "For your review (Jev was below the threshold: judge these yourself from the item; Jev's leaning is shown):",
        false,
    );
    let failures = outcomes
        .iter()
        .filter_map(|(id, outcome)| match outcome {
            Outcome::Failed(error) => Some(format!("- {id}: {error}")),
            Outcome::Answered { .. } => None,
        })
        .collect::<Vec<_>>();
    if !failures.is_empty() {
        let _ = write!(text, "\nFailed:\n{}", failures.join("\n"));
    }
    if call.single && decided == 0 && failed == 0 {
        text.push_str("\n(Not decided: the answer above is Jev's leaning, below the threshold.)");
    }
    text
}

impl ExternalToolDispatcher for JevHost {
    fn validate_external<'a>(
        &'a self,
        plugin_id: &'a str,
        _tool_name: &'a str,
        arguments: &'a Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            if plugin_id != "jev" {
                return Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "jev tool target is unavailable",
                ));
            }
            parse(arguments, self.inner.threshold).map(|_| ())
        })
    }

    fn dispatch_external<'a>(
        &'a self,
        _authorization: &'a ToolDispatchAuthorization,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
        _timeout_ms: u64,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            if plugin_id != "jev" {
                return Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "jev tool target is unavailable",
                ));
            }
            let call = parse(arguments, self.inner.threshold)?;
            if self.inner.key.is_none() {
                return Ok(ToolOutput::Denied {
                    code: ErrorCode::AccessDenied.as_str().to_owned(),
                    reason: "The jev plugin has no TypeSafe key: ask the user to run /login typesafe (or set TYPESAFE_API_KEY).".to_owned(),
                });
            }
            Ok(ToolOutput::ExternalTool {
                plugin_id: "jev".to_owned(),
                tool_name: tool_name.to_owned(),
                payload: json!({ "text": self.run(&call).await }),
                inflight: 1,
            })
        })
    }
}

/// Add the jev tool, when the plugin is on, without changing the other hosts.
pub fn with_tools(inner: Option<ExternalTools>, host: Option<&JevHost>) -> Option<ExternalTools> {
    let Some(host) = host else {
        return inner;
    };
    Some(ExternalTools::new(Arc::new(Combined {
        inner,
        jev: host.tools(),
    })))
}

pub fn with_dispatcher(
    inner: Arc<dyn ExternalToolDispatcher>,
    host: Option<&JevHost>,
) -> Arc<dyn ExternalToolDispatcher> {
    let Some(host) = host else {
        return inner;
    };
    Arc::new(Routed {
        inner,
        jev: host.dispatcher(),
    })
}

struct Combined {
    inner: Option<ExternalTools>,
    jev: ExternalTools,
}

impl ExternalToolCatalog for Combined {
    fn schemas(&self) -> Vec<Value> {
        self.inner
            .iter()
            .flat_map(ExternalTools::schemas)
            .filter(|schema| {
                schema.pointer("/function/name").and_then(Value::as_str) != Some("jev")
            })
            .chain(self.jev.schemas())
            .collect()
    }

    fn resolve(&self, name: &str, arguments: &Value) -> Option<CodingToolAction> {
        if name == "jev" {
            self.jev.resolve(name, arguments)
        } else {
            self.inner
                .as_ref()
                .and_then(|tools| tools.resolve(name, arguments))
        }
    }
}

struct Routed {
    inner: Arc<dyn ExternalToolDispatcher>,
    jev: Arc<dyn ExternalToolDispatcher>,
}

impl Routed {
    fn target(&self, plugin_id: &str) -> &dyn ExternalToolDispatcher {
        if plugin_id == "jev" {
            self.jev.as_ref()
        } else {
            self.inner.as_ref()
        }
    }
}

impl ExternalToolDispatcher for Routed {
    fn validate_external<'a>(
        &'a self,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), HarnessError>> + Send + 'a>>
    {
        self.target(plugin_id)
            .validate_external(plugin_id, tool_name, arguments)
    }

    fn dispatch_external<'a>(
        &'a self,
        authorization: &'a ToolDispatchAuthorization,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
        timeout_ms: u64,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>,
    > {
        self.target(plugin_id).dispatch_external(
            authorization,
            plugin_id,
            tool_name,
            arguments,
            timeout_ms,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Call, JevHost, judge, parse};
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn questions_and_items_are_checked_before_any_call() {
        let questions = json!({"fraud": {"type": "choice", "criteria": {"fraud": "a scam", "legitimate": "not"}}});
        assert!(parse(&json!({"questions": questions, "state": "hi"}), 0.95).is_ok());
        let items = parse(
            &json!({"questions": questions, "items": [{"id": "a", "state": "x"}, {"state": "y"}], "threshold": 0.8}),
            0.95,
        )
        .expect("items");
        assert_eq!(items.items[0].0, "a");
        assert_eq!(items.items[1].0, "2");
        assert!((items.threshold - 0.8).abs() < f64::EPSILON);
        for bad in [
            json!({"state": "x"}),
            json!({"questions": questions}),
            json!({"questions": questions, "state": "x", "items": [{"state": "y"}]}),
            json!({"questions": {"q": {"type": "choice"}}, "state": "x"}),
            json!({"questions": {"q": {"type": "score", "criteria": ["only one"]}}, "state": "x"}),
            json!({"questions": {"q": {"type": "guess"}}, "state": "x"}),
            json!({"questions": questions, "state": "x", "threshold": 2}),
            json!({"questions": questions, "state": "x", "model": "other"}),
        ] {
            assert!(parse(&bad, 0.95).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_answer_is_decided_only_at_the_threshold() {
        let choice = judge(
            "fraud",
            &json!({"type": "choice", "choice": "fraud", "confidence": 0.97, "probabilities": {"fraud": 0.98, "legitimate": 0.02}}),
            0.95,
        );
        assert!(choice.accepted);
        assert_eq!(choice.decision, "fraud");
        let unsure = judge("billing", &json!({"type": "noul", "noul": 0.6}), 0.95);
        assert!(!unsure.accepted, "|2p-1| = 0.2");
        assert!(unsure.decision.starts_with("yes"));
        let sure = judge("billing", &json!({"type": "noul", "noul": 0.01}), 0.95);
        assert!(sure.accepted && sure.decision.starts_with("no"));
    }

    /// TypeSafe on loopback: an item mentioning "scam" is fraud with
    /// confidence 0.99, anything else legitimate at 0.6; one call refuses.
    async fn serve() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buffer = [0_u8; 8192];
                    loop {
                        let Ok(read) = socket.read(&mut buffer).await else {
                            return;
                        };
                        if read == 0 {
                            break;
                        }
                        request.extend_from_slice(&buffer[..read]);
                        let text = String::from_utf8_lossy(&request);
                        if let Some(end) = text.find("\r\n\r\n") {
                            let length = text[..end]
                                .lines()
                                .find_map(|line| {
                                    let (name, value) = line.split_once(':')?;
                                    name.eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse::<usize>().ok())?
                                })
                                .unwrap_or(0);
                            if request.len() >= end + 4 + length {
                                break;
                            }
                        }
                    }
                    let text = String::from_utf8_lossy(&request).into_owned();
                    assert!(
                        text.to_ascii_lowercase().contains("bearer test-key"),
                        "{text}"
                    );
                    let body = text.split("\r\n\r\n").nth(1).unwrap_or_default();
                    let request: serde_json::Value = serde_json::from_str(body).expect("json body");
                    assert_eq!(request["model"], "jev-latest");
                    let state = request["state"].as_str().unwrap_or_default();
                    let (status, answer) = if state.contains("broken") {
                        (
                            "422 Unprocessable Entity",
                            json!({"detail": [{"msg": "bad state"}]}),
                        )
                    } else if state.contains("scam") {
                        (
                            "200 OK",
                            json!({"model": "jev-1.13.0", "answers": {"fraud": {"type": "choice", "choice": "fraud", "confidence": 0.99, "probabilities": {"fraud": 0.995, "legitimate": 0.005}}}, "usage": {"input_tokens": 100, "output_tokens": 1}}),
                        )
                    } else {
                        (
                            "200 OK",
                            json!({"model": "jev-1.13.0", "answers": {"fraud": {"type": "choice", "choice": "legitimate", "confidence": 0.6, "probabilities": {"fraud": 0.2, "legitimate": 0.8}}}, "usage": {"input_tokens": 100, "output_tokens": 1}}),
                        )
                    };
                    let body = answer.to_string();
                    let head = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(head.as_bytes()).await;
                    let _ = socket.write_all(body.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        format!("http://{address}/v1/systemone")
    }

    /// `jev-fraud`'s flow: the confident item is decided, the unsure one comes
    /// back for review with Jev's leaning, a refused one is listed as failed.
    #[tokio::test]
    async fn items_are_decided_or_handed_back_for_review() {
        let endpoint = serve().await;
        let host = JevHost::build(
            Some("test-key".to_owned()),
            endpoint,
            "jev-latest".to_owned(),
            0.95,
        )
        .expect("client");
        let call: Call = parse(
            &json!({
                "questions": {"fraud": {"type": "choice", "instructions": "Fraud or not?", "criteria": {"fraud": "a scam", "legitimate": "not a scam"}}},
                "items": [
                    {"id": "e1", "state": "Send me your bank password, this is not a scam"},
                    {"id": "e2", "state": "Lunch on Friday?"},
                    {"id": "e3", "state": "broken"}
                ]
            }),
            0.95,
        )
        .expect("call");
        let text = host.run(&call).await;
        assert!(
            text.starts_with("jev (jev-1.13.0, threshold 0.95): 3 item(s), 1 decided, 1 for your review, 1 failed"),
            "{text}"
        );
        assert!(
            text.contains("Decided:\n- e1: fraud=fraud (0.99)"),
            "{text}"
        );
        assert!(
            text.contains("- e2: fraud=legitimate (0.60; legitimate 0.80, fraud 0.20)"),
            "{text}"
        );
        assert!(
            text.contains("Failed:\n- e3: TypeSafe answered 422"),
            "{text}"
        );
        assert!(text.contains("200 input tokens"), "{text}");
    }
}
