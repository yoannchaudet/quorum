use std::collections::HashSet;
use std::io;
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::error::AppError;

const CONTRACT_VERSION: u8 = 1;
const COPILOT_PROGRAM: &str = "copilot";
pub(crate) const PLANNING_SAFETY_ARGUMENTS: [&str; 8] = [
    "--plan",
    "--no-custom-instructions",
    "--disable-builtin-mcps",
    "--disallow-temp-dir",
    "--allow-all-tools",
    "--deny-tool=write",
    "--deny-tool=shell",
    "--no-remote-export",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRole {
    Planner,
    Synthesizer,
}

impl AgentRole {
    const fn label(self) -> &'static str {
        match self {
            Self::Planner => "planner",
            Self::Synthesizer => "synthesizer",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSession {
    pub id: Uuid,
    pub name: String,
    pub role: AgentRole,
    pub ordinal: usize,
}

impl AgentSession {
    pub fn planner(work_item_id: &str, planning_run_id: &str, ordinal: usize) -> Self {
        Self::new(work_item_id, planning_run_id, AgentRole::Planner, ordinal)
    }

    pub fn synthesizer(work_item_id: &str, planning_run_id: &str) -> Self {
        Self::new(work_item_id, planning_run_id, AgentRole::Synthesizer, 0)
    }

    pub fn persisted(id: Uuid, name: String, role: AgentRole, ordinal: usize) -> Self {
        Self {
            id,
            name,
            role,
            ordinal,
        }
    }

    fn new(work_item_id: &str, planning_run_id: &str, role: AgentRole, ordinal: usize) -> Self {
        let id = Uuid::new_v4();
        let unique_suffix = id.simple();
        Self {
            id,
            name: format!(
                "quorum-{}-{}-{}-{ordinal}-{unique_suffix}",
                readable_id(work_item_id, "work"),
                readable_id(planning_run_id, "run"),
                role.label(),
            ),
            role,
            ordinal,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NormalizedRequirements {
    title: String,
    markdown: String,
}

impl NormalizedRequirements {
    pub fn new(title: &str, markdown: &str) -> Result<Self, AppError> {
        let title = title.trim();
        let markdown = markdown.replace("\r\n", "\n").replace('\r', "\n");
        if title.is_empty() {
            return Err(AppError::validation(
                "Normalized planning requirements need a title.",
            ));
        }
        Ok(Self {
            title: title.to_owned(),
            markdown,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompletedPlannerArtifact {
    planner_session_name: String,
    model_id: String,
    markdown: String,
}

impl CompletedPlannerArtifact {
    pub fn new(
        planner_session_name: &str,
        model_id: &str,
        markdown: &str,
    ) -> Result<Self, AppError> {
        let planner_session_name = required_value(
            planner_session_name,
            "A completed planner artifact needs its exact session name.",
        )?;
        let model_id = required_value(
            model_id,
            "A completed planner artifact needs its model identifier.",
        )?;
        let markdown = required_value(
            markdown,
            "A completed planner artifact needs Markdown output.",
        )?;
        Ok(Self {
            planner_session_name,
            model_id,
            markdown,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentOutcome {
    Completed,
    NeedsInput,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentQuestion {
    pub id: String,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentEnvelope {
    pub version: u8,
    pub outcome: AgentOutcome,
    #[serde(default)]
    pub questions: Vec<AgentQuestion>,
    pub markdown: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopilotEvent {
    pub sequence: usize,
    pub kind: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopilotRunOutput {
    pub envelope: AgentEnvelope,
    pub events: Vec<CopilotEvent>,
}

pub trait ProcessRunner: Send + Sync {
    fn run(&self, program: &str, arguments: &[String]) -> io::Result<ProcessOutput>;
}

#[derive(Debug, Clone)]
pub struct ProcessOutput {
    pub success: bool,
    pub status: String,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub struct SystemProcessRunner;

impl ProcessRunner for SystemProcessRunner {
    fn run(&self, program: &str, arguments: &[String]) -> io::Result<ProcessOutput> {
        Command::new(program)
            .args(arguments)
            .output()
            .map(|output| ProcessOutput {
                success: output.status.success(),
                status: output.status.to_string(),
                stdout: output.stdout,
                stderr: output.stderr,
            })
    }
}

pub struct CopilotClient<R> {
    runner: R,
}

impl<R: ProcessRunner> CopilotClient<R> {
    pub const fn new(runner: R) -> Self {
        Self { runner }
    }

    pub fn start_planner(
        &self,
        repository_path: &str,
        model: &str,
        session: &AgentSession,
        requirements: &NormalizedRequirements,
    ) -> Result<CopilotRunOutput, AppError> {
        if session.role != AgentRole::Planner {
            return Err(AppError::validation(
                "A planner run requires a planner session.",
            ));
        }
        let prompt = planner_prompt(requirements)?;
        self.start(repository_path, model, session, &prompt)
    }

    pub fn start_synthesizer(
        &self,
        repository_path: &str,
        model: &str,
        session: &AgentSession,
        requirements: &NormalizedRequirements,
        completed_artifacts: &[CompletedPlannerArtifact],
    ) -> Result<CopilotRunOutput, AppError> {
        if session.role != AgentRole::Synthesizer {
            return Err(AppError::validation(
                "A synthesis run requires a synthesizer session.",
            ));
        }
        let prompt = synthesizer_prompt(requirements, completed_artifacts)?;
        self.start(repository_path, model, session, &prompt)
    }

    pub fn resume_named(
        &self,
        repository_path: &str,
        session_name: &str,
        prompt: &str,
    ) -> Result<CopilotRunOutput, AppError> {
        let session_name = required_value(
            session_name,
            "An exact Copilot session name is required to resume planning.",
        )?;
        let prompt = required_value(prompt, "A resume prompt is required.")?;
        let arguments: Vec<String> = common_arguments(repository_path)
            .into_iter()
            .chain([
                format!("--resume={session_name}"),
                "-p".to_owned(),
                contract_prompt(&prompt),
            ])
            .collect();
        self.execute(&arguments)
    }

    fn start(
        &self,
        repository_path: &str,
        model: &str,
        session: &AgentSession,
        prompt: &str,
    ) -> Result<CopilotRunOutput, AppError> {
        let model = required_value(model, "A Copilot model is required.")?;
        let arguments: Vec<String> = common_arguments(repository_path)
            .into_iter()
            .chain([
                "--model".to_owned(),
                model,
                "--session-id".to_owned(),
                session.id.to_string(),
                "--name".to_owned(),
                session.name.clone(),
                "-p".to_owned(),
                contract_prompt(prompt),
            ])
            .collect();
        self.execute(&arguments)
    }

    fn execute(&self, arguments: &[String]) -> Result<CopilotRunOutput, AppError> {
        let output = self
            .runner
            .run(COPILOT_PROGRAM, arguments)
            .map_err(|error| process_start_error(&error))?;
        if !output.success {
            return Err(process_failure(&output));
        }
        let stdout = String::from_utf8(output.stdout).map_err(|error| {
            AppError::external(format!(
                "Copilot CLI returned non-UTF-8 JSONL output at byte {}. Update Copilot CLI and retry.",
                error.utf8_error().valid_up_to()
            ))
        })?;
        parse_jsonl(&stdout)
    }
}

fn common_arguments(repository_path: &str) -> Vec<String> {
    let mut arguments = vec![
        "-C".to_owned(),
        repository_path.to_owned(),
        "--output-format".to_owned(),
        "json".to_owned(),
        "--stream=on".to_owned(),
        "--silent".to_owned(),
        "--no-ask-user".to_owned(),
    ];
    arguments.extend(PLANNING_SAFETY_ARGUMENTS.map(str::to_owned));
    arguments
}

fn planner_prompt(requirements: &NormalizedRequirements) -> Result<String, AppError> {
    let requirements = serde_json::to_string_pretty(requirements).map_err(|error| {
        AppError::external(format!(
            "Quorum could not serialize normalized planning requirements: {error}"
        ))
    })?;
    Ok(format!(
        "Act as one isolated planner. Base your work only on the normalized requirements below \
         and repository content you inspect yourself. Do not use or infer output from any other \
         planner. Produce a concrete implementation plan; do not implement it.\n\n\
         NORMALIZED_REQUIREMENTS_JSON\n{requirements}"
    ))
}

fn synthesizer_prompt(
    requirements: &NormalizedRequirements,
    completed_artifacts: &[CompletedPlannerArtifact],
) -> Result<String, AppError> {
    if completed_artifacts.is_empty() {
        return Err(AppError::validation(
            "Synthesis requires at least one completed planner artifact.",
        ));
    }
    let mut session_names = HashSet::new();
    for artifact in completed_artifacts {
        if !session_names.insert(&artifact.planner_session_name) {
            return Err(AppError::validation(format!(
                "Planner artifact {} was supplied more than once.",
                artifact.planner_session_name
            )));
        }
    }
    let requirements = serde_json::to_string_pretty(requirements).map_err(|error| {
        AppError::external(format!(
            "Quorum could not serialize normalized planning requirements: {error}"
        ))
    })?;
    let artifacts = serde_json::to_string_pretty(completed_artifacts).map_err(|error| {
        AppError::external(format!(
            "Quorum could not serialize completed planner artifacts: {error}"
        ))
    })?;
    Ok(format!(
        "Act as the synthesizer. Reconcile the completed planner artifacts into one coherent, \
         actionable implementation plan for the normalized requirements. Preserve useful \
         disagreements as explicit trade-offs and do not implement the plan.\n\n\
         NORMALIZED_REQUIREMENTS_JSON\n{requirements}\n\n\
         COMPLETED_PLANNER_ARTIFACTS_JSON\n{artifacts}"
    ))
}

fn contract_prompt(prompt: &str) -> String {
    format!(
        "{prompt}\n\n\
         SAFETY AND OUTPUT CONTRACT\n\
         You are planning only. Never create, modify, rename, or delete files in the target \
         repository, and never run shell commands. Return the final result through the assistant \
         response as exactly one JSON object with no Markdown fence or surrounding prose:\n\
         {{\"version\":{CONTRACT_VERSION},\"outcome\":\"completed|needs_input|blocked\",\
         \"questions\":[{{\"id\":\"stable-id\",\"prompt\":\"question\"}}],\
         \"markdown\":\"plan or null\",\"error\":\"reason or null\"}}.\n\
         Use needs_input with one or more precise questions when a user decision is required. \
         Use completed with non-empty Markdown when planning is complete. Use blocked with a \
         concrete error only for an unrecoverable external problem."
    )
}

fn parse_jsonl(output: &str) -> Result<CopilotRunOutput, AppError> {
    let mut events = Vec::new();
    let mut envelopes = Vec::new();
    let mut malformed_envelope = None;
    for (line_index, line) in output.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let payload: Value = serde_json::from_str(line).map_err(|error| {
            AppError::external(format!(
                "Copilot CLI returned malformed JSONL on line {}: {error}. Update Copilot CLI and retry.",
                line_index + 1
            ))
        })?;
        collect_envelopes(&payload, &mut envelopes, &mut malformed_envelope);
        events.push(CopilotEvent {
            sequence: events.len(),
            kind: event_kind(&payload),
            payload,
        });
    }
    if events.is_empty() {
        return Err(AppError::external(
            "Copilot CLI returned no JSONL events. Verify authentication with `copilot login` and retry.",
        ));
    }
    if envelopes.is_empty() {
        if let Some(error) = malformed_envelope {
            return Err(AppError::external(format!(
                "Copilot CLI returned a malformed planning envelope: {error}"
            )));
        }
        return Err(AppError::external(
            "Copilot CLI completed without the required structured planning envelope. Update Copilot CLI and retry.",
        ));
    }
    let envelope = envelopes.pop().expect("checked non-empty");
    if envelopes.iter().any(|candidate| candidate != &envelope) {
        return Err(AppError::external(
            "Copilot CLI returned conflicting structured planning envelopes.",
        ));
    }
    Ok(CopilotRunOutput {
        envelope: validate_envelope(envelope)?,
        events,
    })
}

fn collect_envelopes(
    value: &Value,
    envelopes: &mut Vec<AgentEnvelope>,
    malformed: &mut Option<String>,
) {
    match value {
        Value::String(content) => {
            let content = content.trim();
            if content.starts_with('{') {
                match serde_json::from_str::<Value>(content) {
                    Ok(nested) => collect_envelopes(&nested, envelopes, malformed),
                    Err(error)
                        if content.contains("\"version\"") || content.contains("\"outcome\"") =>
                    {
                        *malformed = Some(error.to_string());
                    }
                    Err(_) => {}
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_envelopes(value, envelopes, malformed);
            }
        }
        Value::Object(values) => {
            if values.contains_key("version") || values.contains_key("outcome") {
                match serde_json::from_value::<AgentEnvelope>(value.clone()) {
                    Ok(envelope) => envelopes.push(envelope),
                    Err(error) => *malformed = Some(error.to_string()),
                }
            } else {
                for value in values.values() {
                    collect_envelopes(value, envelopes, malformed);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn event_kind(value: &Value) -> Option<String> {
    let object = value.as_object()?;
    ["type", "event", "kind"]
        .into_iter()
        .find_map(|key| object.get(key).and_then(Value::as_str))
        .map(str::to_owned)
}

fn validate_envelope(envelope: AgentEnvelope) -> Result<AgentEnvelope, AppError> {
    if envelope.version != CONTRACT_VERSION {
        return Err(AppError::external(format!(
            "Copilot returned unsupported planning contract version {}; expected {CONTRACT_VERSION}.",
            envelope.version
        )));
    }
    let has_markdown = envelope
        .markdown
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    let has_error = envelope
        .error
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    let mut question_ids = HashSet::new();
    for question in &envelope.questions {
        if question.id.trim().is_empty() || question.prompt.trim().is_empty() {
            return Err(AppError::external(
                "Copilot returned a planning question without a stable ID and non-empty prompt.",
            ));
        }
        if !question_ids.insert(question.id.as_str()) {
            return Err(AppError::external(format!(
                "Copilot returned duplicate planning question ID {}.",
                question.id
            )));
        }
    }
    match envelope.outcome {
        AgentOutcome::Completed
            if !has_markdown || has_error || !envelope.questions.is_empty() =>
        {
            Err(AppError::external(
                "Copilot's completed planning envelope must contain Markdown and no questions or error.",
            ))
        }
        AgentOutcome::NeedsInput if envelope.questions.is_empty() || has_error => {
            Err(AppError::external(
                "Copilot's needs_input envelope must contain questions and no error.",
            ))
        }
        AgentOutcome::Blocked if !has_error || !envelope.questions.is_empty() => {
            Err(AppError::external(
                "Copilot's blocked planning envelope must contain an error and no questions.",
            ))
        }
        _ => Ok(envelope),
    }
}

fn process_start_error(error: &io::Error) -> AppError {
    match error.kind() {
        io::ErrorKind::NotFound => AppError::external(
            "GitHub Copilot CLI executable `copilot` was not found on PATH. Install Copilot CLI, verify `copilot --version`, and retry.",
        ),
        io::ErrorKind::PermissionDenied => AppError::external(
            "GitHub Copilot CLI could not be executed because permission was denied. Check the executable permissions and retry.",
        ),
        _ => AppError::external(format!(
            "GitHub Copilot CLI could not be started: {error}. Verify `copilot --version` and retry."
        )),
    }
}

fn process_failure(output: &ProcessOutput) -> AppError {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    let lowercase = detail.to_ascii_lowercase();
    if [
        "not authenticated",
        "not logged in",
        "authentication required",
        "unauthorized",
        "copilot login",
        "status 401",
    ]
    .iter()
    .any(|marker| lowercase.contains(marker))
    {
        return AppError::external(format!(
            "GitHub Copilot CLI is not authenticated (status {}). Run `copilot login`, confirm access, and retry.{}",
            output.status,
            formatted_detail(detail)
        ));
    }
    AppError::external(format!(
        "GitHub Copilot CLI planning exited with status {}.{}",
        output.status,
        formatted_detail(detail)
    ))
}

fn formatted_detail(detail: &str) -> String {
    if detail.is_empty() {
        " Check Copilot CLI logs for details.".to_owned()
    } else {
        let mut characters = detail.chars();
        let mut shortened = characters.by_ref().take(1_000).collect::<String>();
        if characters.next().is_some() {
            shortened.push('…');
        }
        format!(" Copilot reported: {shortened}")
    }
}

fn required_value(value: &str, message: &str) -> Result<String, AppError> {
    let value = value.trim();
    if value.is_empty() {
        Err(AppError::validation(message))
    } else {
        Ok(value.to_owned())
    }
}

fn readable_id(value: &str, fallback: &str) -> String {
    let readable = value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(12)
        .flat_map(char::to_lowercase)
        .collect::<String>();
    if readable.is_empty() {
        fallback.to_owned()
    } else {
        readable
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::Mutex;

    use super::{
        AgentEnvelope, AgentOutcome, AgentSession, CompletedPlannerArtifact, CopilotClient,
        NormalizedRequirements, ProcessOutput, ProcessRunner,
    };

    struct FakeRunner {
        calls: Mutex<Vec<Vec<String>>>,
        result: Mutex<Option<io::Result<ProcessOutput>>>,
    }

    impl FakeRunner {
        fn successful(stdout: &str) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                result: Mutex::new(Some(Ok(ProcessOutput {
                    success: true,
                    status: "exit status: 0".to_owned(),
                    stdout: stdout.as_bytes().to_vec(),
                    stderr: Vec::new(),
                }))),
            }
        }

        fn failing(status: &str, stderr: &str) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                result: Mutex::new(Some(Ok(ProcessOutput {
                    success: false,
                    status: status.to_owned(),
                    stdout: Vec::new(),
                    stderr: stderr.as_bytes().to_vec(),
                }))),
            }
        }
    }

    impl ProcessRunner for &FakeRunner {
        fn run(&self, program: &str, arguments: &[String]) -> io::Result<ProcessOutput> {
            assert_eq!(program, "copilot");
            self.calls.lock().expect("calls").push(arguments.to_vec());
            self.result
                .lock()
                .expect("result")
                .take()
                .expect("one process call")
        }
    }

    fn completed_jsonl() -> String {
        let envelope = AgentEnvelope {
            version: 1,
            outcome: AgentOutcome::Completed,
            questions: Vec::new(),
            markdown: Some("# Plan".to_owned()),
            error: None,
        };
        format!(
            "{{\"type\":\"assistant.delta\",\"data\":{{\"content\":\"working\"}}}}\n\
             {{\"type\":\"assistant.message\",\"data\":{{\"content\":{}}}}}",
            serde_json::to_string(&serde_json::to_string(&envelope).expect("envelope"))
                .expect("event content")
        )
    }

    #[test]
    fn creates_collision_resistant_readable_names_for_each_role() {
        let first = AgentSession::planner("work-item-id", "planning-run-id", 2);
        let second = AgentSession::planner("work-item-id", "planning-run-id", 2);
        let synthesizer = AgentSession::synthesizer("work-item-id", "planning-run-id");
        assert_ne!(first.id, second.id);
        assert_ne!(first.name, second.name);
        assert!(first
            .name
            .starts_with("quorum-workitemid-planningruni-planner-2-"));
        assert!(first.name.ends_with(&first.id.simple().to_string()));
        assert!(synthesizer.name.contains("-synthesizer-0-"));
        assert!(synthesizer
            .name
            .ends_with(&synthesizer.id.simple().to_string()));
    }

    #[test]
    fn starts_isolated_planner_in_read_only_plan_mode() {
        let runner = FakeRunner::successful(&completed_jsonl());
        let client = CopilotClient::new(&runner);
        let session = AgentSession::planner("work", "run", 0);
        let requirements = NormalizedRequirements::new("Feature", "# Feature\r\n\r\nDetails")
            .expect("requirements");
        let output = client
            .start_planner(
                "/Users/example/repository",
                "gpt-test",
                &session,
                &requirements,
            )
            .expect("planner");
        assert_eq!(output.envelope.markdown.as_deref(), Some("# Plan"));
        assert_eq!(output.events.len(), 2);
        assert_eq!(output.events[0].kind.as_deref(), Some("assistant.delta"));

        let calls = runner.calls.lock().expect("calls");
        let arguments = &calls[0];
        for required in [
            "--plan",
            "--stream=on",
            "--no-ask-user",
            "--no-custom-instructions",
            "--disable-builtin-mcps",
            "--disallow-temp-dir",
            "--allow-all-tools",
            "--deny-tool=write",
            "--deny-tool=shell",
            "--no-remote-export",
        ] {
            assert!(arguments.iter().any(|argument| argument == required));
        }
        assert!(arguments
            .windows(2)
            .any(|pair| pair == ["--output-format", "json"]));
        assert!(arguments
            .windows(2)
            .any(|pair| pair == ["--session-id", &session.id.to_string()]));
        assert!(arguments
            .windows(2)
            .any(|pair| pair == ["--name", &session.name]));
        let prompt = arguments.last().expect("prompt");
        assert!(prompt.contains("one isolated planner"));
        assert!(prompt.contains("\"markdown\": \"# Feature\\n\\nDetails\""));
        assert!(!prompt.contains("COMPLETED_PLANNER_ARTIFACTS_JSON"));
    }

    #[test]
    fn accepts_title_only_requirements_for_issues_without_a_body() {
        let requirements =
            NormalizedRequirements::new("Title-only issue", "").expect("requirements");
        let runner = FakeRunner::successful(&completed_jsonl());
        let session = AgentSession::planner("work", "run", 0);
        CopilotClient::new(&runner)
            .start_planner(
                "/Users/example/repository",
                "gpt-test",
                &session,
                &requirements,
            )
            .expect("planner");
        let calls = runner.calls.lock().expect("calls");
        let prompt = calls[0].last().expect("prompt");
        assert!(prompt.contains("\"title\": \"Title-only issue\""));
        assert!(prompt.contains("\"markdown\": \"\""));
    }

    #[test]
    fn synthesizer_receives_completed_artifacts() {
        let runner = FakeRunner::successful(&completed_jsonl());
        let client = CopilotClient::new(&runner);
        let session = AgentSession::synthesizer("work", "run");
        let requirements =
            NormalizedRequirements::new("Feature", "# Requirements").expect("requirements");
        let artifact = CompletedPlannerArtifact::new("planner-session", "model-one", "# Candidate")
            .expect("artifact");
        client
            .start_synthesizer(
                "/Users/example/repository",
                "synthesis-model",
                &session,
                &requirements,
                &[artifact],
            )
            .expect("synthesis");
        let calls = runner.calls.lock().expect("calls");
        let prompt = calls[0].last().expect("prompt");
        assert!(prompt.contains("COMPLETED_PLANNER_ARTIFACTS_JSON"));
        assert!(prompt.contains("\"planner_session_name\": \"planner-session\""));
        assert!(prompt.contains("\"markdown\": \"# Candidate\""));
    }

    #[test]
    fn resumes_the_exact_named_session() {
        let runner = FakeRunner::successful(&completed_jsonl());
        let client = CopilotClient::new(&runner);
        let session = AgentSession::planner("work", "run", 0);
        client
            .resume_named(
                "/Users/example/repository",
                &session.name,
                "Continue with this answer.",
            )
            .expect("resume");
        let calls = runner.calls.lock().expect("calls");
        let arguments = &calls[0];
        assert!(arguments
            .iter()
            .any(|argument| argument == &format!("--resume={}", session.name)));
        assert!(!arguments.iter().any(|argument| argument == "--continue"));
        assert!(!arguments.iter().any(|argument| argument == "--session-id"));
    }

    #[test]
    fn reports_missing_cli_and_authentication_failures() {
        let missing = FakeRunner {
            calls: Mutex::new(Vec::new()),
            result: Mutex::new(Some(Err(io::Error::new(
                io::ErrorKind::NotFound,
                "missing",
            )))),
        };
        let client = CopilotClient::new(&missing);
        let error = client
            .resume_named("/Users/example/repository", "session", "continue")
            .expect_err("missing");
        assert!(error.message.contains("not found on PATH"));

        let unauthenticated =
            FakeRunner::failing("exit status: 1", "Not authenticated. Run copilot login.");
        let error = CopilotClient::new(&unauthenticated)
            .resume_named("/Users/example/repository", "session", "continue")
            .expect_err("authentication");
        assert!(error.message.contains("Run `copilot login`"));
        assert!(error.message.contains("exit status: 1"));
    }

    #[test]
    fn reports_nonzero_exit_and_malformed_jsonl() {
        let failed = FakeRunner::failing("exit status: 9", "model unavailable");
        let error = CopilotClient::new(&failed)
            .resume_named("/Users/example/repository", "session", "continue")
            .expect_err("failure");
        assert!(error.message.contains("exit status: 9"));
        assert!(error.message.contains("model unavailable"));

        let malformed = FakeRunner::successful("{\"type\":\"start\"}\nnot-json");
        let error = CopilotClient::new(&malformed)
            .resume_named("/Users/example/repository", "session", "continue")
            .expect_err("malformed");
        assert!(error.message.contains("malformed JSONL on line 2"));
    }

    #[test]
    fn rejects_malformed_or_incoherent_envelopes() {
        let malformed = FakeRunner::successful(
            r#"{"type":"assistant.message","content":"{\"version\":1,\"outcome\":\"done\"}"}"#,
        );
        let error = CopilotClient::new(&malformed)
            .resume_named("/Users/example/repository", "session", "continue")
            .expect_err("malformed envelope");
        assert!(
            error.message.contains("malformed planning envelope"),
            "{}",
            error.message
        );

        let incomplete = FakeRunner::successful(
            r#"{"version":1,"outcome":"needs_input","questions":[],"markdown":null,"error":null}"#,
        );
        let error = CopilotClient::new(&incomplete)
            .resume_named("/Users/example/repository", "session", "continue")
            .expect_err("incoherent envelope");
        assert!(error.message.contains("needs_input"));
    }

    #[test]
    fn rejects_duplicate_synthesis_artifacts_before_starting_process() {
        let runner = FakeRunner::successful(&completed_jsonl());
        let client = CopilotClient::new(&runner);
        let session = AgentSession::synthesizer("work", "run");
        let requirements =
            NormalizedRequirements::new("Feature", "# Requirements").expect("requirements");
        let artifact = CompletedPlannerArtifact::new("planner-session", "model-one", "# Candidate")
            .expect("artifact");
        let error = client
            .start_synthesizer(
                "/Users/example/repository",
                "model",
                &session,
                &requirements,
                &[artifact.clone(), artifact],
            )
            .expect_err("duplicate");
        assert!(error.message.contains("supplied more than once"));
        assert!(runner.calls.lock().expect("calls").is_empty());
    }
}
