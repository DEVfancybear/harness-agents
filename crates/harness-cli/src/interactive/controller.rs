//! State reducer for the interactive app.
//!
//! The controller owns the phase, the transcript and the compiled effects. It
//! never touches a terminal, so every rule below is unit tested without a PTY;
//! the host translates effects into terminal calls.

use harness_types::InputId;

use super::bootstrap::LaunchContext;
use super::events::{AppPhase, Key, SessionEvent};
use super::input::{InputOutcome, LineEditor};
use super::service::{SessionChannel, SessionPort, SubmitRequest};
use super::view;

/// Exit code for a normal quit.
pub const EXIT_SUCCESS: u8 = 0;

/// One thing the host must do after a controller step.
///
/// The exit code is a plain number so effects stay comparable in tests; the host
/// maps it to a process exit code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    /// Append one complete line.
    WriteLine(String),
    /// Append text exactly as given, for incremental output.
    WritePartial(String),
    /// Reprint the prompt with the current buffer.
    RedrawPrompt,
    /// Leave the app with this exit code.
    Exit(u8),
}

/// Interactive app state and its transitions.
pub struct InteractiveController {
    phase: AppPhase,
    setup_required: bool,
    header: Vec<String>,
    setup_hint: Option<String>,
    transcript: Vec<String>,
    editor: LineEditor,
    service: Box<dyn SessionPort>,
    channel: SessionChannel,
    pending_text: String,
    active_input: Option<InputId>,
}

impl InteractiveController {
    #[must_use]
    pub fn new(
        context: &LaunchContext,
        service: Box<dyn SessionPort>,
        channel: SessionChannel,
    ) -> Self {
        let mut header = context.header_lines();
        header.push(format!("Service: {}", service.label()));
        Self {
            // The app boots before it can render; boot_lines performs the
            // transition once the header has actually been produced.
            phase: AppPhase::Booting,
            setup_required: context.setup_required,
            header,
            setup_hint: context.setup_hint(),
            transcript: Vec::new(),
            editor: LineEditor::new(),
            service,
            channel,
            pending_text: String::new(),
            active_input: None,
        }
    }

    #[cfg(test)]
    #[must_use]
    pub const fn phase(&self) -> AppPhase {
        self.phase
    }

    #[cfg(test)]
    #[must_use]
    pub fn transcript(&self) -> &[String] {
        &self.transcript
    }

    /// Prompt text for the current phase and buffer.
    #[must_use]
    pub fn prompt(&self) -> String {
        view::prompt_line(self.phase, self.editor.buffer())
    }

    /// Everything the host prints once, before the first prompt.
    ///
    /// Rendering the boot header is the transition out of the booting phase, so
    /// the prompt that follows already shows the state the app is really in.
    pub fn boot_lines(&mut self) -> Vec<String> {
        self.phase = if self.setup_required {
            AppPhase::SetupRequired
        } else {
            AppPhase::Ready
        };
        let mut lines = vec![String::new()];
        lines.extend(self.header.iter().cloned());
        if let Some(hint) = &self.setup_hint {
            lines.push(hint.clone());
        }
        lines.push("Nhập yêu cầu. /help trợ giúp · /status chẩn đoán · /exit thoát".to_owned());
        lines
    }

    /// Apply one key.
    pub fn handle_key(&mut self, key: Key) -> Vec<Effect> {
        match self.editor.handle(key) {
            InputOutcome::Unchanged => Vec::new(),
            InputOutcome::Redraw => vec![Effect::RedrawPrompt],
            InputOutcome::Exit => {
                self.phase = AppPhase::Closed;
                vec![Effect::Exit(EXIT_SUCCESS)]
            }
            InputOutcome::Interrupt => self.interrupt(),
            InputOutcome::Submit(text) => self.submit(text),
        }
    }

    /// Move everything the session port produced into effects.
    pub fn pump_events(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        let events = self.channel.drain();
        if events.is_empty() {
            return effects;
        }
        for event in events {
            match event {
                SessionEvent::Accepted { input_id } => {
                    self.flush_stream(&mut effects);
                    let line =
                        view::run_line(&format!("accepted {}", view::short_id(input_id.as_str())));
                    self.push_line(&mut effects, line);
                }
                SessionEvent::TextDelta { text } => self.pending_text.push_str(&text),
                SessionEvent::ToolStarted { name, summary } => {
                    self.flush_stream(&mut effects);
                    let line = view::tool_line(&name, &summary);
                    self.push_line(&mut effects, line);
                }
                SessionEvent::ToolSettled { name, ok } => {
                    self.flush_stream(&mut effects);
                    let line = view::tool_line(&name, if ok { "ok" } else { "failed" });
                    self.push_line(&mut effects, line);
                }
                SessionEvent::RunTerminal { outcome } => {
                    self.flush_stream(&mut effects);
                    let line = view::run_line(&outcome.label());
                    self.push_line(&mut effects, line);
                    self.finish_run();
                }
                SessionEvent::RecoverableError { message } => {
                    self.flush_stream(&mut effects);
                    let line = format!("[error] {message}");
                    self.push_line(&mut effects, line);
                    self.finish_run();
                }
            }
        }
        // Text that arrived in this cycle is shown now, not after the run ends.
        self.flush_stream(&mut effects);
        effects.push(Effect::RedrawPrompt);
        effects
    }

    fn submit(&mut self, text: String) -> Vec<Effect> {
        if text.starts_with('/') {
            return self.command(&text);
        }
        if self.phase.has_active_run() {
            return vec![
                Effect::WriteLine(
                    "a run is already active; wait for it or press Ctrl-C to cancel".to_owned(),
                ),
                Effect::RedrawPrompt,
            ];
        }
        let input_id = InputId::generate();
        let echo = format!("> {text}");
        self.pending_text.clear();
        self.active_input = Some(input_id.clone());
        self.phase = AppPhase::Running;
        self.service.submit(SubmitRequest { input_id, text });
        let mut effects = Vec::new();
        self.push_line(&mut effects, echo);
        effects.push(Effect::RedrawPrompt);
        effects
    }

    fn interrupt(&mut self) -> Vec<Effect> {
        if self.phase.has_active_run() {
            self.service.cancel();
            self.phase = AppPhase::Canceling;
            return vec![
                Effect::WriteLine("^C canceling the active run...".to_owned()),
                Effect::RedrawPrompt,
            ];
        }
        self.editor.clear();
        vec![Effect::WriteLine(String::new()), Effect::RedrawPrompt]
    }

    fn command(&mut self, line: &str) -> Vec<Effect> {
        let mut parts = line.split_whitespace();
        let name = parts.next().unwrap_or_default();
        let argument = parts.next();
        let mut effects = Vec::new();
        match name {
            "/exit" | "/quit" => {
                if self.phase.has_active_run() {
                    self.service.cancel();
                    self.push_line(
                        &mut effects,
                        "^C canceling the active run before exit".to_owned(),
                    );
                }
                self.phase = AppPhase::Closed;
                effects.push(Effect::Exit(EXIT_SUCCESS));
                return effects;
            }
            "/help" => {
                for help in view::help_lines() {
                    self.push_line(&mut effects, help);
                }
            }
            "/status" => {
                let header = self.header.clone();
                for line in header {
                    self.push_line(&mut effects, line);
                }
                let phase = format!("Phase:   {}", self.phase.label());
                self.push_line(&mut effects, phase);
            }
            "/config" => {
                let selected: Vec<String> = self
                    .header
                    .iter()
                    .filter(|line| line.starts_with("Config:") || line.starts_with("Data:"))
                    .cloned()
                    .collect();
                for line in selected {
                    self.push_line(&mut effects, line);
                }
            }
            "/new" => {
                let text = if self.phase.has_active_run() {
                    "cannot start a new session while a run is active; press Ctrl-C to cancel it first"
                        .to_owned()
                } else {
                    "session lifecycle (new session, checkpoint, close) arrives with HA_LAUNCH H05; this session stays open"
                        .to_owned()
                };
                self.push_line(&mut effects, text);
            }
            "/model" => {
                let label = self.service.label();
                let text = format!(
                    "model resolution arrives with HA_LAUNCH H04; current backend: {label}"
                );
                self.push_line(&mut effects, text);
            }
            "/resume" => {
                let text = match argument {
                    Some(session) => format!(
                        "resume of session {session} arrives with HA_LAUNCH H05; no state was changed"
                    ),
                    None => "usage: /resume <session-id>".to_owned(),
                };
                self.push_line(&mut effects, text);
            }
            other => {
                let text =
                    format!("unknown command {other}; /help lists what this revision supports");
                self.push_line(&mut effects, text);
            }
        }
        effects.push(Effect::RedrawPrompt);
        effects
    }

    fn flush_stream(&mut self, effects: &mut Vec<Effect>) {
        if self.pending_text.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.pending_text);
        self.transcript.push(text.clone());
        effects.push(Effect::WritePartial(text));
    }

    fn push_line(&mut self, effects: &mut Vec<Effect>, line: String) {
        self.transcript.push(line.clone());
        effects.push(Effect::WriteLine(line));
    }

    fn finish_run(&mut self) {
        self.active_input = None;
        self.phase = if self.setup_required {
            AppPhase::SetupRequired
        } else {
            AppPhase::Ready
        };
    }
}

#[cfg(test)]
mod tests {
    use super::{Effect, InteractiveController};
    use crate::interactive::bootstrap::{self, LaunchContext, LaunchRequest};
    use crate::interactive::events::{AppPhase, Key, RunOutcome, SessionEvent};
    use crate::interactive::paths::{HostPlatform, LaunchEnvironment};
    use crate::interactive::service::{
        FixtureService, PendingService, SessionChannel, SessionPort, SubmitRequest,
    };
    use std::sync::{Arc, Mutex};

    /// Test port that records what the controller admitted.
    #[derive(Clone, Default)]
    struct RecordingPort {
        submitted: Arc<Mutex<Vec<String>>>,
        cancels: Arc<Mutex<usize>>,
    }

    impl SessionPort for RecordingPort {
        fn label(&self) -> String {
            "recording test port".to_owned()
        }
        fn submit(&mut self, request: SubmitRequest) {
            self.submitted
                .lock()
                .expect("submission lock")
                .push(request.text);
        }
        fn cancel(&mut self) {
            *self.cancels.lock().expect("cancel lock") += 1;
        }
    }

    /// Test port that ignores everything; the test drives the channel itself.
    struct SilentPort;

    impl SessionPort for SilentPort {
        fn label(&self) -> String {
            "silent test port".to_owned()
        }
        fn submit(&mut self, _request: SubmitRequest) {}
        fn cancel(&mut self) {}
    }

    struct Bench {
        _temp: tempfile::TempDir,
        context: LaunchContext,
    }

    fn bench(configured: bool) -> Bench {
        let temp = tempfile::tempdir().expect("temp root");
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&home).expect("fixture home");
        std::fs::create_dir_all(&project).expect("fixture project");
        if configured {
            std::fs::write(home.join("config.toml"), "schema_version = 1\n")
                .expect("fixture config");
        }
        let context = bootstrap::resolve(LaunchRequest {
            cwd: None,
            caller_dir: project,
            platform: HostPlatform::current(),
            environment: LaunchEnvironment::from_pairs([
                ("HA_HOME", home.to_string_lossy().into_owned()),
                ("DEEPSEEK_API_KEY", "fixture-secret".to_owned()),
            ]),
            explicit_data_dir: None,
        })
        .expect("context resolves");
        Bench {
            _temp: temp,
            context,
        }
    }

    fn submit_text(controller: &mut InteractiveController, text: &str) -> Vec<Effect> {
        for character in text.chars() {
            let _ = controller.handle_key(Key::Char(character));
        }
        controller.handle_key(Key::Enter)
    }

    fn lines(effects: &[Effect]) -> Vec<String> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::WriteLine(line) => Some(line.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn h03_one_admission_per_message_and_a_running_run_refuses_a_second() {
        let bench = bench(true);
        let port = RecordingPort::default();
        let recorded = port.clone();
        let mut controller =
            InteractiveController::new(&bench.context, Box::new(port), SessionChannel::new());
        assert_eq!(controller.phase(), AppPhase::Booting);
        let _ = controller.boot_lines();
        assert_eq!(controller.phase(), AppPhase::Ready);

        let effects = submit_text(&mut controller, "first request");
        assert!(lines(&effects).iter().any(|line| line == "> first request"));
        assert_eq!(controller.phase(), AppPhase::Running);
        assert_eq!(recorded.submitted.lock().expect("lock").len(), 1);

        let refusal = submit_text(&mut controller, "second request");
        assert!(
            lines(&refusal)
                .iter()
                .any(|line| line.contains("already active")),
            "{refusal:?}"
        );
        assert_eq!(
            recorded.submitted.lock().expect("lock").len(),
            1,
            "a second input must not be admitted while a run is active"
        );
    }

    #[test]
    fn h03_ctrl_c_cancels_a_run_and_clears_an_idle_prompt() {
        let bench = bench(true);
        let port = RecordingPort::default();
        let recorded = port.clone();
        let mut controller =
            InteractiveController::new(&bench.context, Box::new(port), SessionChannel::new());

        submit_text(&mut controller, "long task");
        let effects = controller.handle_key(Key::Interrupt);
        assert!(
            lines(&effects)
                .iter()
                .any(|line| line.contains("canceling")),
            "{effects:?}"
        );
        assert_eq!(controller.phase(), AppPhase::Canceling);
        assert_eq!(*recorded.cancels.lock().expect("lock"), 1);

        let mut idle = InteractiveController::new(
            &bench.context,
            Box::new(RecordingPort::default()),
            SessionChannel::new(),
        );
        let _ = idle.boot_lines();
        for character in "typo".chars() {
            let _ = idle.handle_key(Key::Char(character));
        }
        assert!(idle.prompt().ends_with("typo"));
        assert_eq!(idle.phase(), AppPhase::Ready);
        let cleared = idle.handle_key(Key::Interrupt);
        assert_eq!(idle.prompt(), "> ", "an idle Ctrl-C clears the input");
        assert!(lines(&cleared).iter().any(String::is_empty));
    }

    #[test]
    fn h03_fixture_run_streams_text_before_tools_and_returns_to_ready() {
        let bench = bench(true);
        let channel = SessionChannel::new();
        let service = FixtureService::new(channel.sender());
        let mut controller = InteractiveController::new(&bench.context, Box::new(service), channel);
        let _ = controller.boot_lines();

        submit_text(&mut controller, "fix the parser");
        let effects = controller.pump_events();
        assert_eq!(
            effects.last(),
            Some(&Effect::RedrawPrompt),
            "the prompt is reprinted after output"
        );
        let streamed: String = effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::WritePartial(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            streamed,
            "fixture answer for: fix the parser (no model was called)"
        );
        assert!(
            effects
                .iter()
                .position(|effect| matches!(effect, Effect::WritePartial(_)))
                .expect("streamed text")
                < effects
                    .iter()
                    .position(|effect| matches!(effect, Effect::WriteLine(line) if line.starts_with("[tool]")))
                    .expect("tool line"),
            "text must be rendered before the tool lines"
        );

        let transcript = controller.transcript().join("\n");
        assert!(
            transcript.contains("[tool] search_text failed"),
            "{transcript}"
        );
        assert!(transcript.contains("[tool] apply_patch ok"), "{transcript}");
        assert!(transcript.contains("[run] done"), "{transcript}");
        assert_eq!(controller.phase(), AppPhase::Ready);
    }

    #[test]
    fn h03_text_and_terminal_events_are_rendered_before_the_run_ends() {
        let bench = bench(true);
        let channel = SessionChannel::new();
        let sender = channel.sender();
        let mut controller =
            InteractiveController::new(&bench.context, Box::new(SilentPort), channel);
        let _ = controller.boot_lines();

        submit_text(&mut controller, "stream please");
        sender
            .send(SessionEvent::TextDelta {
                text: "partial answer".to_owned(),
            })
            .expect("delta is delivered");
        let effects = controller.pump_events();
        assert_eq!(
            effects.first(),
            Some(&Effect::WritePartial("partial answer".to_owned()))
        );
        assert_eq!(
            controller.phase(),
            AppPhase::Running,
            "text must be visible while the run is still active"
        );

        sender
            .send(SessionEvent::RunTerminal {
                outcome: RunOutcome::Done,
            })
            .expect("terminal event is delivered");
        let effects = controller.pump_events();
        assert!(lines(&effects).iter().any(|line| line == "[run] done"));
        assert_eq!(controller.phase(), AppPhase::Ready);
    }

    #[test]
    fn h03_pending_service_says_connection_pending_and_keeps_setup_state() {
        let bench = bench(false);
        let channel = SessionChannel::new();
        let service = PendingService::new(channel.sender());
        let mut controller = InteractiveController::new(&bench.context, Box::new(service), channel);
        let _ = controller.boot_lines();
        assert_eq!(controller.phase(), AppPhase::SetupRequired);

        submit_text(&mut controller, "do work");
        let effects = controller.pump_events();
        let rendered = lines(&effects).join("\n");
        assert!(rendered.contains("connection pending"), "{rendered}");
        assert_eq!(
            controller.phase(),
            AppPhase::SetupRequired,
            "the setup state is not lost by a failed turn"
        );
    }

    #[test]
    fn h03_slash_commands_are_parsed_and_staged_features_stay_honest() {
        let bench = bench(true);
        let mut controller = InteractiveController::new(
            &bench.context,
            Box::new(RecordingPort::default()),
            SessionChannel::new(),
        );
        let _ = controller.boot_lines();

        let help = lines(&submit_text(&mut controller, "/help")).join("\n");
        assert!(help.contains("/status") && help.contains("/exit"), "{help}");

        let status = lines(&submit_text(&mut controller, "/status")).join("\n");
        assert!(status.contains("Project:"), "{status}");
        assert!(status.contains("Phase:   ready"), "{status}");

        let model = lines(&submit_text(&mut controller, "/model")).join("\n");
        assert!(model.contains("H04"), "{model}");
        let config = lines(&submit_text(&mut controller, "/config")).join("\n");
        assert!(config.contains("Config:"), "{config}");
        let resume = lines(&submit_text(&mut controller, "/resume")).join("\n");
        assert!(resume.contains("usage"), "{resume}");
        let resume = lines(&submit_text(&mut controller, "/resume session_01")).join("\n");
        assert!(resume.contains("H05"), "{resume}");
        let unknown = lines(&submit_text(&mut controller, "/nope")).join("\n");
        assert!(unknown.contains("unknown command"), "{unknown}");
        let fresh = lines(&submit_text(&mut controller, "/new")).join("\n");
        assert!(fresh.contains("H05"), "{fresh}");

        submit_text(&mut controller, "work");
        let busy = lines(&submit_text(&mut controller, "/new")).join("\n");
        assert!(busy.contains("cannot start a new session"), "{busy}");

        let exit = submit_text(&mut controller, "/exit");
        assert!(matches!(exit.last(), Some(Effect::Exit(_))), "{exit:?}");
        assert!(
            lines(&exit).iter().any(|line| line.contains("canceling")),
            "an active run is canceled before exit: {exit:?}"
        );
    }
}
