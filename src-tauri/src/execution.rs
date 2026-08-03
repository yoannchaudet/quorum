use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use fs2::FileExt;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use ts_rs::TS;
use uuid::Uuid;

use crate::error::{AppError, StoreError};
use crate::state::AppStore;

const MAX_REMEDIATION_ITERATIONS: usize = 3;
const MAX_CAPTURE_BYTES: usize = 512 * 1024;
const MAX_PROCESS_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_PERSISTED_COMMAND_BYTES: usize = 64 * 1024;
const MAX_REVIEW_DIFF_BYTES: usize = 384 * 1024;
const MAX_EVIDENCE_BYTES: usize = 32 * 1024 * 1024;
const PROCESS_OUTPUT_CHANNEL_CAPACITY: usize = 16;
const REVIEW_CONTRACT_VERSION: u8 = 1;
const OWNED_PROCESS_MARKER: &str = "--quorum-owned-process";
const RUNTIME_DIRECTORY: &str = ".quorum-runtime";
const TERMINATION_GRACE: Duration = Duration::from_millis(500);
const EVIDENCE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct StartExecutionRequest {
    pub queue_entry_id: String,
    pub idempotency_key: String,
}

fn open_lock_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ResumeExecutionRequest {
    pub run_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct CancelExecutionRequest {
    pub run_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ResolveExecutionFindingRequest {
    pub run_id: String,
    pub finding_id: String,
    pub disposition_note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ExecutionRunDto {
    pub id: String,
    pub work_item_id: String,
    pub plan_id: String,
    pub queue_entry_id: String,
    pub phase: String,
    pub outcome: String,
    pub status: String,
    pub current_step: String,
    pub base_commit: Option<String>,
    pub branch_name: String,
    pub worktree_path: String,
    pub builder_session_name: String,
    pub builder_model: String,
    pub reviewer_session_name: String,
    pub reviewer_model: String,
    pub verification_program: Option<String>,
    pub verification_arguments: Vec<String>,
    pub iteration: usize,
    pub max_iterations: usize,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ExecutionAttemptDto {
    pub id: String,
    pub number: usize,
    pub reason: String,
    pub status: String,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub started_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ExecutionLogDto {
    pub id: String,
    pub command_id: String,
    pub phase: String,
    pub program: String,
    pub stream: String,
    pub sequence: usize,
    pub text: String,
    pub truncated: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ExecutionFindingDto {
    pub id: String,
    pub external_id: String,
    pub severity: String,
    pub title: String,
    pub body: String,
    pub path: Option<String>,
    pub line: Option<usize>,
    pub status: String,
    pub disposition_note: Option<String>,
    pub first_seen_iteration: usize,
    pub last_seen_iteration: usize,
    pub created_at: String,
    pub updated_at: String,
    pub resolved_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ExecutionPhaseEventDto {
    pub id: String,
    pub sequence: usize,
    pub event_kind: String,
    #[ts(type = "unknown")]
    pub payload: Value,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ExecutionDetailDto {
    pub run: ExecutionRunDto,
    pub attempts: Vec<ExecutionAttemptDto>,
    pub recent_logs: Vec<ExecutionLogDto>,
    pub findings: Vec<ExecutionFindingDto>,
    pub recent_events: Vec<ExecutionPhaseEventDto>,
    pub blocking_finding_count: usize,
    pub can_resume: bool,
    pub can_cancel: bool,
    pub delivery_ready: bool,
}

#[derive(Default)]
pub struct ExecutionSupervisor {
    controls: Mutex<HashMap<String, Arc<RunControl>>>,
}

impl ExecutionSupervisor {
    fn begin(&self, run_id: &str) -> Result<Arc<RunControl>, AppError> {
        let mut controls = self
            .controls
            .lock()
            .map_err(|_| AppError::database("The execution supervisor lock became unavailable."))?;
        if controls.contains_key(run_id) {
            return Err(AppError::conflict(
                "This execution run already has an owned worker.",
            ));
        }
        let control = Arc::new(RunControl::default());
        controls.insert(run_id.to_owned(), Arc::clone(&control));
        Ok(control)
    }

    fn finish(&self, run_id: &str, control: &Arc<RunControl>) {
        if let Ok(mut controls) = self.controls.lock() {
            if controls
                .get(run_id)
                .is_some_and(|registered| Arc::ptr_eq(registered, control))
            {
                controls.remove(run_id);
            }
        }
    }

    fn control(&self, run_id: &str) -> Option<Arc<RunControl>> {
        self.controls
            .lock()
            .ok()
            .and_then(|controls| controls.get(run_id).cloned())
    }
}

struct SupervisorRegistration {
    supervisor: Arc<ExecutionSupervisor>,
    run_id: String,
    control: Arc<RunControl>,
}

impl Drop for SupervisorRegistration {
    fn drop(&mut self) {
        self.supervisor.finish(&self.run_id, &self.control);
    }
}

struct RunControl {
    cancelled: AtomicBool,
    child: Mutex<Option<OwnedChild>>,
}

impl Default for RunControl {
    fn default() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            child: Mutex::new(None),
        }
    }
}

struct OwnedChild {
    child: Child,
    #[cfg(unix)]
    process_group: u32,
    termination_started: bool,
}

impl RunControl {
    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.terminate_child();
    }

    fn install_child(&self, child: Child) -> io::Result<()> {
        let mut owned = self
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if owned.is_some() {
            return Err(io::Error::other(
                "execution attempted to own more than one child process",
            ));
        }
        #[cfg(unix)]
        let process_group = child.id();
        *owned = Some(OwnedChild {
            child,
            #[cfg(unix)]
            process_group,
            termination_started: false,
        });
        if self.cancelled() {
            terminate_owned_child(owned.as_mut().expect("installed child"));
        }
        Ok(())
    }

    fn terminate_child(&self) {
        let mut owned = self
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(child) = owned.as_mut() {
            terminate_owned_child(child);
        }
    }

    fn try_wait(&self) -> io::Result<Option<ExitStatus>> {
        self.child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
            .map_or(Ok(None), |owned| owned.child.try_wait())
    }

    fn finish_child(&self) {
        let mut owned = self
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(child) = owned.as_mut() {
            ensure_owned_process_group_empty(child);
        }
        owned.take();
    }

    fn force_cleanup(&self) {
        let mut owned = self
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(child) = owned.as_mut() {
            terminate_owned_child(child);
            ensure_owned_process_group_empty(child);
            let _ = child.child.wait();
        }
        owned.take();
    }
}

impl Drop for RunControl {
    fn drop(&mut self) {
        self.force_cleanup();
    }
}

#[cfg(unix)]
fn process_group_alive(process_group: u32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", &format!("-{process_group}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(unix)]
fn signal_process_group(process_group: u32, signal: &str) {
    let _ = Command::new("/bin/kill")
        .args([signal, &format!("-{process_group}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(unix)]
fn wait_for_process_group_exit(process_group: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while process_group_alive(process_group) {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
    true
}

#[cfg(unix)]
fn terminate_owned_child(owned: &mut OwnedChild) {
    if owned.termination_started {
        return;
    }
    owned.termination_started = true;
    if !process_group_alive(owned.process_group) {
        return;
    }
    signal_process_group(owned.process_group, "-TERM");
    if !wait_for_process_group_exit(owned.process_group, TERMINATION_GRACE) {
        signal_process_group(owned.process_group, "-KILL");
        let _ = wait_for_process_group_exit(owned.process_group, TERMINATION_GRACE);
    }
}

#[cfg(unix)]
fn ensure_owned_process_group_empty(owned: &mut OwnedChild) {
    if process_group_alive(owned.process_group) {
        terminate_owned_child(owned);
    }
    while process_group_alive(owned.process_group) {
        signal_process_group(owned.process_group, "-KILL");
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(not(unix))]
fn terminate_owned_child(owned: &mut OwnedChild) {
    if !owned.termination_started {
        owned.termination_started = true;
        let _ = owned.child.kill();
    }
}

#[cfg(not(unix))]
fn ensure_owned_process_group_empty(owned: &mut OwnedChild) {
    terminate_owned_child(owned);
    let _ = owned.child.wait();
}

#[derive(Debug, Clone)]
struct ProcessRequest {
    program: String,
    arguments: Vec<String>,
    cwd: PathBuf,
    environment: Vec<(String, String)>,
    clear_git_environment: bool,
    stdout_path: Option<PathBuf>,
    lease_path: Option<PathBuf>,
    untrusted: bool,
}

impl ProcessRequest {
    fn new(program: String, arguments: Vec<String>, cwd: PathBuf) -> Self {
        Self {
            program,
            arguments,
            cwd,
            environment: Vec::new(),
            clear_git_environment: false,
            stdout_path: None,
            lease_path: None,
            untrusted: false,
        }
    }
}

#[derive(Debug)]
struct ProcessChunk {
    stream: &'static str,
    bytes: Vec<u8>,
}

#[derive(Debug)]
struct ProcessResult {
    success: bool,
    exit_code: Option<i32>,
    status: String,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    capture_truncated: bool,
}

trait ExecutionProcessRunner: Send + Sync {
    fn run(
        &self,
        request: &ProcessRequest,
        control: &RunControl,
        output: &mut dyn FnMut(ProcessChunk),
    ) -> io::Result<ProcessResult>;
}

#[derive(Default)]
struct SystemExecutionProcessRunner;

impl ExecutionProcessRunner for SystemExecutionProcessRunner {
    fn run(
        &self,
        request: &ProcessRequest,
        control: &RunControl,
        output: &mut dyn FnMut(ProcessChunk),
    ) -> io::Result<ProcessResult> {
        let mut command = owned_command(request)?;
        command
            .current_dir(&request.cwd)
            .stdin(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(path) = &request.stdout_path {
            command.stdout(open_output_file(path)?);
        } else {
            command.stdout(Stdio::piped());
        }

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn()?;
        let stdout = child.stdout.take();
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("could not capture process stderr"))?;
        let (sender, receiver) = mpsc::sync_channel(PROCESS_OUTPUT_CHANNEL_CAPACITY);
        let stdout_reader = stdout.map(|stdout| spawn_reader(stdout, "stdout", sender.clone()));
        let stderr_reader = spawn_reader(stderr, "stderr", sender);
        control.install_child(child)?;
        let mut child_guard = InstalledChildGuard::new(control);

        let mut stdout_tail = Vec::new();
        let mut captured_stderr = Vec::new();
        let mut capture_truncated = false;
        let mut total_output = 0_usize;
        let mut output_limit_exceeded = false;
        let mut status = None;
        let mut readers_finished = false;
        loop {
            if control.cancelled() {
                control.terminate_child();
            }
            match receiver.recv_timeout(Duration::from_millis(40)) {
                Ok(mut chunk) => {
                    if !output_limit_exceeded {
                        let remaining = MAX_PROCESS_OUTPUT_BYTES.saturating_sub(total_output);
                        let chunk_length = chunk.bytes.len();
                        let retained = remaining.min(chunk_length);
                        if retained > 0 {
                            chunk.bytes.truncate(retained);
                            total_output += retained;
                            if chunk.stream == "stdout" {
                                capture_truncated |=
                                    append_tail(&mut stdout_tail, &chunk.bytes, MAX_CAPTURE_BYTES);
                            } else {
                                capture_prefix(
                                    &mut captured_stderr,
                                    &chunk.bytes,
                                    &mut capture_truncated,
                                );
                            }
                            output(chunk);
                        }
                        if retained < chunk_length {
                            output_limit_exceeded = true;
                            capture_truncated = true;
                            control.terminate_child();
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => readers_finished = true,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if status.is_none() {
                if let Some(completed) = control.try_wait()? {
                    control.finish_child();
                    child_guard.disarm();
                    status = Some(completed);
                }
            }
            if status.is_some() && readers_finished {
                break;
            }
        }
        if let Some(stdout_reader) = stdout_reader {
            let _ = stdout_reader.join();
        }
        let _ = stderr_reader.join();
        let status = status.expect("installed child completed before its output readers");
        let rendered_status = if output_limit_exceeded {
            format!("{status} (process output limit exceeded)")
        } else {
            status.to_string()
        };
        Ok(ProcessResult {
            success: status.success() && !control.cancelled() && !output_limit_exceeded,
            exit_code: status.code(),
            status: rendered_status,
            stdout: stdout_tail,
            stderr: captured_stderr,
            capture_truncated,
        })
    }
}

fn append_tail(destination: &mut Vec<u8>, bytes: &[u8], limit: usize) -> bool {
    if bytes.len() >= limit {
        destination.clear();
        destination.extend_from_slice(&bytes[bytes.len() - limit..]);
        return true;
    }
    let overflow = destination
        .len()
        .saturating_add(bytes.len())
        .saturating_sub(limit);
    if overflow > 0 {
        destination.drain(..overflow);
    }
    destination.extend_from_slice(bytes);
    overflow > 0
}

struct InstalledChildGuard<'a> {
    control: &'a RunControl,
    armed: bool,
}

impl<'a> InstalledChildGuard<'a> {
    fn new(control: &'a RunControl) -> Self {
        Self {
            control,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for InstalledChildGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.control.force_cleanup();
        }
    }
}

fn open_output_file(path: &Path) -> io::Result<File> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(io::Error::other(format!(
                "refusing to redirect output through non-regular path {}",
                path.display()
            )));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

#[allow(clippy::unnecessary_wraps)]
fn owned_command(request: &ProcessRequest) -> io::Result<Command> {
    #[cfg(test)]
    let mut command = {
        let mut command = Command::new(&request.program);
        command.args(&request.arguments);
        command
    };
    #[cfg(not(test))]
    let mut command = {
        let lease_path = request.lease_path.as_ref().ok_or_else(|| {
            io::Error::other("execution command is missing its durable ownership lease")
        })?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg(OWNED_PROCESS_MARKER)
            .arg(lease_path)
            .arg("--")
            .arg(&request.program)
            .args(&request.arguments);
        command
    };
    if request.clear_git_environment {
        clear_git_environment(&mut command);
    }
    command.envs(
        request
            .environment
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    );
    Ok(command)
}

pub fn owned_process_entry() -> Option<std::process::ExitCode> {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new(OWNED_PROCESS_MARKER)) {
        return None;
    }
    Some(match run_owned_process(arguments.collect()) {
        Ok(0) => std::process::ExitCode::SUCCESS,
        Ok(_) => std::process::ExitCode::FAILURE,
        Err(error) => {
            eprintln!("Quorum refused to launch an unowned execution process: {error}");
            std::process::ExitCode::FAILURE
        }
    })
}

fn run_owned_process(arguments: Vec<OsString>) -> io::Result<i32> {
    let mut arguments = arguments.into_iter();
    let lease_path = arguments
        .next()
        .ok_or_else(|| io::Error::other("owned process is missing its lease path"))?;
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("--")) {
        return Err(io::Error::other(
            "owned process is missing its argument separator",
        ));
    }
    let program = arguments
        .next()
        .ok_or_else(|| io::Error::other("owned process is missing its program"))?;
    let lease = open_lock_file(Path::new(&lease_path))?;
    FileExt::try_lock_exclusive(&lease).map_err(|error| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("another Quorum process owns this execution lease: {error}"),
        )
    })?;
    #[cfg(unix)]
    let owned_process_group = current_owned_process_group();
    let status = Command::new(program).args(arguments).status()?;
    #[cfg(unix)]
    if let Some(process_group) = owned_process_group {
        terminate_process_group_members_except(process_group, std::process::id())?;
    }
    Ok(status.code().unwrap_or(1))
}

#[cfg(unix)]
fn current_owned_process_group() -> Option<u32> {
    let process_id = std::process::id();
    let output = Command::new("/bin/ps")
        .args(["-o", "pgid=", "-p", &process_id.to_string()])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let process_group = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .ok()?;
    (process_group == process_id).then_some(process_group)
}

#[cfg(unix)]
fn process_group_members(process_group: u32, excluding: u32) -> io::Result<Vec<u32>> {
    let output = Command::new("/bin/ps")
        .args(["-axo", "pid=,pgid=,ppid=,comm="])
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(
            "could not inspect the owned process group",
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let process_id = fields.next()?.parse::<u32>().ok()?;
            let candidate_group = fields.next()?.parse::<u32>().ok()?;
            let parent_id = fields.next()?.parse::<u32>().ok()?;
            let command = fields.next().unwrap_or_default();
            let is_probe = parent_id == excluding
                && Path::new(command)
                    .file_name()
                    .is_some_and(|name| name == "ps");
            (candidate_group == process_group && process_id != excluding && !is_probe)
                .then_some(process_id)
        })
        .collect())
}

#[cfg(unix)]
fn signal_processes(processes: &[u32], signal: &str) {
    for process in processes {
        let _ = Command::new("/bin/kill")
            .args([signal, &process.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(unix)]
fn wait_for_other_process_group_members(
    process_group: u32,
    excluding: u32,
    timeout: Duration,
) -> io::Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        if process_group_members(process_group, excluding)?.is_empty() {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
fn terminate_process_group_members_except(process_group: u32, excluding: u32) -> io::Result<()> {
    let members = process_group_members(process_group, excluding)?;
    if members.is_empty() {
        return Ok(());
    }
    signal_processes(&members, "-TERM");
    if !wait_for_other_process_group_members(process_group, excluding, TERMINATION_GRACE)? {
        loop {
            let members = process_group_members(process_group, excluding)?;
            if members.is_empty() {
                break;
            }
            signal_processes(&members, "-KILL");
            thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(())
}

fn spawn_reader(
    mut reader: impl Read + Send + 'static,
    stream: &'static str,
    sender: mpsc::SyncSender<ProcessChunk>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = vec![0_u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(length) => {
                    if sender
                        .send(ProcessChunk {
                            stream,
                            bytes: buffer[..length].to_vec(),
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    })
}

fn capture_prefix(destination: &mut Vec<u8>, bytes: &[u8], truncated: &mut bool) {
    let remaining = MAX_CAPTURE_BYTES.saturating_sub(destination.len());
    if remaining == 0 {
        *truncated = true;
        return;
    }
    let retained = remaining.min(bytes.len());
    destination.extend_from_slice(&bytes[..retained]);
    *truncated |= retained < bytes.len();
}

#[derive(Clone)]
pub struct ExecutionService {
    store: Arc<AppStore>,
    supervisor: Arc<ExecutionSupervisor>,
    runner: Arc<dyn ExecutionProcessRunner>,
}

impl ExecutionService {
    pub fn system(store: Arc<AppStore>, supervisor: Arc<ExecutionSupervisor>) -> Self {
        Self {
            store,
            supervisor,
            runner: Arc::new(SystemExecutionProcessRunner),
        }
    }

    #[cfg(test)]
    #[allow(dead_code)]
    fn with_runner(
        store: Arc<AppStore>,
        supervisor: Arc<ExecutionSupervisor>,
        runner: Arc<dyn ExecutionProcessRunner>,
    ) -> Self {
        Self {
            store,
            supervisor,
            runner,
        }
    }

    #[allow(clippy::too_many_lines)]
    pub fn start(&self, request: &StartExecutionRequest) -> Result<ExecutionDetailDto, AppError> {
        let queue_entry_id = required(
            &request.queue_entry_id,
            "A queued plan is required to start execution.",
        )?;
        let idempotency_key = required(
            &request.idempotency_key,
            "An execution idempotency key is required.",
        )?;
        if let Some(run_id) = self.store.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT run_id FROM execution_runs WHERE idempotency_key = ?1",
                    [&idempotency_key],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(Into::into)
        })? {
            return self.detail(&run_id);
        }

        let target = self.load_start_target(&queue_entry_id)?;
        let run_id = Uuid::new_v4().to_string();
        let short_run_id = short_id(&run_id);
        let readable = readable_slug(&target.title);
        let branch_name = format!("quorum/{readable}-{short_run_id}");
        let worktree_path = self
            .store
            .app_data_dir()
            .join("worktrees")
            .join(format!("{readable}-{short_run_id}"));
        let builder_session_id = Uuid::new_v4().to_string();
        let reviewer_session_id = Uuid::new_v4().to_string();
        let ownership_token = Uuid::new_v4().to_string();
        let builder_session_name = format!("quorum-{readable}-{short_run_id}-builder");
        let reviewer_session_name = format!("quorum-{readable}-{short_run_id}-reviewer");
        let preflight = preflight(&target.repository_path, &branch_name, &worktree_path);
        let timestamp = now();
        let initial = if preflight.error.is_some() {
            ("blocked", "blocked", "preparing")
        } else {
            ("running", "starting", "preparing")
        };
        let attempt_id = Uuid::new_v4().to_string();

        self.store.with_connection(|connection| {
            let transaction = connection.unchecked_transaction()?;
            let existing_run: Option<String> = transaction
                .query_row(
                    "SELECT run_id FROM queue_entries WHERE id = ?1",
                    [&queue_entry_id],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            if let Some(existing_run) = existing_run {
                return Err(StoreError::App(AppError::conflict(format!(
                    "This queued plan already belongs to execution run {existing_run}."
                ))));
            }
            transaction.execute(
                "INSERT INTO runs (
                   id, work_item_id, plan_id, phase, outcome, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, 'building', ?4, ?5, ?5)",
                params![
                    run_id,
                    target.work_item_id,
                    target.plan_id,
                    initial.0,
                    timestamp
                ],
            )?;
            transaction.execute(
                "INSERT INTO execution_runs (
                   run_id, queue_entry_id, source_repository_path, base_commit,
                   branch_name, worktree_path, ownership_token, copilot_program,
                   builder_session_id, builder_session_name, builder_model,
                   reviewer_session_id, reviewer_session_name, reviewer_model,
                   verification_program, verification_args_json, status, current_step,
                   iteration, max_iterations, idempotency_key, error_code, error_message,
                   created_at, updated_at, completed_at
                 ) VALUES (
                   ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                   ?14, ?15, ?16, ?17, ?18, 0, ?19, ?20, ?21, ?22, ?23, ?23, ?24
                 )",
                params![
                    run_id,
                    queue_entry_id,
                    target.repository_path,
                    preflight.base_commit,
                    branch_name,
                    worktree_path.to_string_lossy(),
                    ownership_token,
                    preflight.copilot_program,
                    builder_session_id,
                    builder_session_name,
                    target.builder_model,
                    reviewer_session_id,
                    reviewer_session_name,
                    target.reviewer_model,
                    preflight
                        .verification
                        .as_ref()
                        .map(|verification| verification.program.as_str()),
                    preflight
                        .verification
                        .as_ref()
                        .map(|verification| serde_json::to_string(&verification.arguments))
                        .transpose()
                        .map_err(|error| {
                            StoreError::App(AppError::database(format!(
                                "Could not persist verification arguments: {error}"
                            )))
                        })?,
                    initial.1,
                    initial.2,
                    MAX_REMEDIATION_ITERATIONS,
                    idempotency_key,
                    preflight.error.as_ref().map(|error| error.code.as_str()),
                    preflight.error.as_ref().map(|error| error.message.as_str()),
                    timestamp,
                    preflight.error.as_ref().map(|_| timestamp.as_str())
                ],
            )?;
            transaction.execute(
                "INSERT INTO execution_attempts (
                   id, run_id, number, reason, status, error_code, error_message,
                   started_at, completed_at
                 ) VALUES (?1, ?2, 1, 'start', ?3, ?4, ?5, ?6, ?7)",
                params![
                    attempt_id,
                    run_id,
                    if preflight.error.is_some() {
                        "blocked"
                    } else {
                        "running"
                    },
                    preflight.error.as_ref().map(|error| error.code.as_str()),
                    preflight.error.as_ref().map(|error| error.message.as_str()),
                    timestamp,
                    preflight.error.as_ref().map(|_| timestamp.as_str())
                ],
            )?;
            transaction.execute(
                "UPDATE queue_entries SET run_id = ?2, updated_at = ?3 WHERE id = ?1",
                params![queue_entry_id, run_id, timestamp],
            )?;
            append_event(
                &transaction,
                &run_id,
                if preflight.error.is_some() {
                    "execution_blocked"
                } else {
                    "execution_started"
                },
                &json!({
                    "branchName": branch_name,
                    "worktreePath": worktree_path,
                    "baseCommit": preflight.base_commit,
                    "diagnostic": preflight.error.as_ref().map(|error| &error.message),
                }),
                &timestamp,
            )?;
            transaction.commit()?;
            Ok(())
        })?;

        if preflight.error.is_none() {
            self.launch_worker(&run_id)?;
        }
        self.detail(&run_id)
    }

    pub fn latest_for_work_item(
        &self,
        work_item_id: &str,
    ) -> Result<Option<ExecutionDetailDto>, AppError> {
        let work_item_id = required(work_item_id, "A work item ID is required.")?;
        let run_id = self.store.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT runs.id
                     FROM runs
                     JOIN execution_runs ON execution_runs.run_id = runs.id
                     WHERE runs.work_item_id = ?1
                     ORDER BY runs.created_at DESC LIMIT 1",
                    [&work_item_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(Into::into)
        })?;
        run_id.map(|run_id| self.detail(&run_id)).transpose()
    }

    pub fn detail(&self, run_id: &str) -> Result<ExecutionDetailDto, AppError> {
        let run_id = required(run_id, "An execution run ID is required.")?;
        self.store
            .with_connection(|connection| load_detail(connection, &run_id))
    }

    pub fn resume(&self, request: &ResumeExecutionRequest) -> Result<ExecutionDetailDto, AppError> {
        let run_id = required(&request.run_id, "An execution run ID is required.")?;
        if !self.prepare_resume(&run_id)? {
            return self.detail(&run_id);
        }
        self.store.with_connection(|connection| {
            let transaction = connection.unchecked_transaction()?;
            let record = transaction
                .query_row(
                    "SELECT status, current_step, iteration, max_iterations, error_code
                     FROM execution_runs WHERE run_id = ?1",
                    [&run_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, usize>(2)?,
                            row.get::<_, usize>(3)?,
                            row.get::<_, Option<String>>(4)?,
                        ))
                    },
                )
                .optional()?
                .ok_or_else(|| {
                    StoreError::App(AppError::not_found("The execution run could not be found."))
                })?;
            if record.0 != "blocked" && record.0 != "failed" {
                return Err(StoreError::App(AppError::conflict(
                    "Only a blocked or failed execution can be resumed.",
                )));
            }
            let open_blocking = blocking_findings(&transaction, &run_id)?;
            if !resume_allowed(record.4.as_deref(), record.2, record.3, open_blocking) {
                return Err(StoreError::App(AppError::conflict(
                    "This execution cannot resume until its blocking findings are dispositioned or its bounded remediation state changes.",
                )));
            }
            if record.1 == "complete" {
                return Err(StoreError::App(AppError::conflict(
                    "This execution run has already completed.",
                )));
            }
            let number: usize = transaction.query_row(
                "SELECT COALESCE(MAX(number), 0) + 1
                 FROM execution_attempts WHERE run_id = ?1",
                [&run_id],
                |row| row.get(0),
            )?;
            let timestamp = now();
            rotate_unconfirmed_session(&transaction, &run_id, &record.1, &timestamp)?;
            transaction.execute(
                "INSERT INTO execution_attempts (
                   id, run_id, number, reason, status, started_at
                 ) VALUES (?1, ?2, ?3, 'resume', 'running', ?4)",
                params![Uuid::new_v4().to_string(), run_id, number, timestamp],
            )?;
            let status = status_for_step(&record.1);
            transaction.execute(
                "UPDATE execution_runs
                 SET status = ?2, error_code = NULL, error_message = NULL,
                     completed_at = NULL, updated_at = ?3
                 WHERE run_id = ?1",
                params![run_id, status, timestamp],
            )?;
            transaction.execute(
                "UPDATE runs
                 SET phase = ?2, outcome = 'running', updated_at = ?3 WHERE id = ?1",
                params![run_id, phase_for_step(&record.1), timestamp],
            )?;
            append_event(
                &transaction,
                &run_id,
                "execution_resumed",
                &json!({"attempt": number, "step": record.1}),
                &timestamp,
            )?;
            transaction.commit()?;
            Ok(())
        })?;
        self.launch_worker(&run_id)?;
        self.detail(&run_id)
    }

    pub fn cancel(&self, request: &CancelExecutionRequest) -> Result<ExecutionDetailDto, AppError> {
        let run_id = required(&request.run_id, "An execution run ID is required.")?;
        let control = self.supervisor.control(&run_id).ok_or_else(|| {
            AppError::conflict(
                "This execution run has no process owned by the current Quorum session.",
            )
        })?;
        self.store.with_connection(|connection| {
            let transaction = connection.unchecked_transaction()?;
            let timestamp = now();
            let changed = transaction.execute(
                "UPDATE execution_runs
                 SET status = 'cancelling', updated_at = ?2
                 WHERE run_id = ?1
                   AND status IN (
                     'starting', 'building', 'verifying', 'reviewing', 'remediating'
                   )",
                params![run_id, timestamp],
            )?;
            if changed == 0 {
                return Err(StoreError::App(AppError::conflict(
                    "This execution run is not cancellable.",
                )));
            }
            append_event(
                &transaction,
                &run_id,
                "cancellation_requested",
                &json!({"message": "Cancellation was requested for this run's owned process."}),
                &timestamp,
            )?;
            transaction.commit()?;
            Ok(())
        })?;
        control.cancel();
        self.detail(&run_id)
    }

    pub fn resolve_finding(
        &self,
        request: &ResolveExecutionFindingRequest,
    ) -> Result<ExecutionDetailDto, AppError> {
        let run_id = required(&request.run_id, "An execution run ID is required.")?;
        let finding_id = required(&request.finding_id, "A finding ID is required.")?;
        let note = required(
            &request.disposition_note,
            "Explain why this blocking finding is explicitly resolved.",
        )?;
        let should_mark_ready = self.store.with_connection(|connection| {
            let transaction = connection.unchecked_transaction()?;
            let (status, error_code): (String, Option<String>) = transaction
                .query_row(
                    "SELECT status, error_code FROM execution_runs WHERE run_id = ?1",
                    [&run_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .ok_or_else(|| {
                    StoreError::App(AppError::not_found("The execution run could not be found."))
                })?;
            if status != "blocked" {
                return Err(StoreError::App(AppError::conflict(
                    "Blocking findings can only be resolved after execution has stopped.",
                )));
            }
            let timestamp = now();
            let changed = transaction.execute(
                "UPDATE execution_findings
                 SET status = 'resolved', disposition_note = ?3,
                     resolved_at = ?4, updated_at = ?4
                 WHERE id = ?1 AND run_id = ?2
                   AND severity = 'blocking' AND status = 'open'",
                params![finding_id, run_id, note, timestamp],
            )?;
            if changed == 0 {
                return Err(StoreError::App(AppError::conflict(
                    "The open blocking finding could not be resolved.",
                )));
            }
            append_event(
                &transaction,
                &run_id,
                "finding_resolved",
                &json!({"findingId": finding_id, "note": note}),
                &timestamp,
            )?;
            let should_mark_ready = blocking_findings(&transaction, &run_id)? == 0
                && matches!(
                    error_code.as_deref(),
                    Some("blocking_findings" | "no_material_fix")
                );
            transaction.commit()?;
            Ok(should_mark_ready)
        })?;
        if should_mark_ready {
            let snapshot = self.snapshot(&run_id)?;
            let expected = snapshot
                .verified_state_digest
                .as_deref()
                .filter(|digest| snapshot.reviewed_state_digest.as_deref() == Some(*digest));
            let current = expected
                .ok_or_else(|| {
                    AppError::conflict(
                        "Delivery readiness has no matching verified and reviewed full-state digests.",
                    )
                })
                .and_then(|expected| {
                    Self::base_evidence(&snapshot, &RunControl::default())
                        .map_err(|error| AppError::conflict(error.message))
                        .map(|evidence| (expected.to_owned(), evidence))
                });
            match current {
                Ok((expected, evidence)) if evidence.digest == expected => {
                    let timestamp = now();
                    self.store.with_connection(|connection| {
                        let transaction = connection.unchecked_transaction()?;
                        mark_ready(&transaction, &run_id, &expected, &timestamp)?;
                        transaction.commit()?;
                        Ok(())
                    })?;
                }
                Ok(_) | Err(_) => {
                    self.rewind_delivery_state(&run_id)?;
                    self.mark_blocked(
                        &run_id,
                        "state_changed_after_review",
                        "The managed worktree no longer matches the persisted verified and reviewed full-state digests. The finding disposition was saved, but delivery remains blocked until verification and review rerun.",
                    )?;
                }
            }
        }
        self.detail(&run_id)
    }

    fn load_start_target(&self, queue_entry_id: &str) -> Result<StartTarget, AppError> {
        self.store.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT queue_entries.work_item_id, queue_entries.plan_id,
                            work_items.title, repositories.root_path,
                            (SELECT model_id FROM model_assignments
                             WHERE role = 'implementation' AND position = 0),
                            (SELECT model_id FROM model_assignments
                             WHERE role = 'adversary' AND position = 0)
                     FROM queue_entries
                     JOIN work_items ON work_items.id = queue_entries.work_item_id
                     JOIN plans ON plans.id = queue_entries.plan_id
                     JOIN repositories ON repositories.id = work_items.repository_id
                     WHERE queue_entries.id = ?1
                       AND queue_entries.scheduling_status = 'queued'
                       AND queue_entries.run_id IS NULL
                       AND work_items.lifecycle_status = 'open'
                       AND repositories.archived_at IS NULL
                       AND plans.queue_eligible_at IS NOT NULL
                       AND (
                         plans.approval_policy = 'not_required'
                         OR plans.approval_status = 'approved'
                       )",
                    [queue_entry_id],
                    |row| {
                        Ok(StartTarget {
                            work_item_id: row.get(0)?,
                            plan_id: row.get(1)?,
                            title: row.get(2)?,
                            repository_path: row.get(3)?,
                            builder_model: row.get(4)?,
                            reviewer_model: row.get(5)?,
                        })
                    },
                )
                .optional()?
                .ok_or_else(|| {
                    StoreError::App(AppError::conflict(
                        "Execution requires an eligible queued plan that has not already started.",
                    ))
                })
        })
    }

    fn launch_worker(&self, run_id: &str) -> Result<(), AppError> {
        let control = self.supervisor.begin(run_id)?;
        let service = self.clone();
        let owned_run_id = run_id.to_owned();
        let thread_control = Arc::clone(&control);
        let supervisor = Arc::clone(&self.supervisor);
        thread::Builder::new()
            .name(format!("quorum-execution-{}", short_id(run_id)))
            .spawn(move || {
                let _registration = SupervisorRegistration {
                    supervisor,
                    run_id: owned_run_id.clone(),
                    control: Arc::clone(&thread_control),
                };
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    service.run_worker(&owned_run_id, &thread_control);
                }))
                .is_err()
                {
                    thread_control.force_cleanup();
                    let _ = service.mark_blocked(
                        &owned_run_id,
                        "worker_panicked",
                        "The execution worker stopped unexpectedly. Its owned process group was terminated before ownership was released.",
                    );
                }
            })
            .map_err(|error| {
                self.supervisor.finish(run_id, &control);
                let message = format!("Quorum could not start the execution worker: {error}");
                let _ = self.mark_blocked(run_id, "worker_start_failed", &message);
                AppError::external(message)
            })?;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn prepare_resume(&self, run_id: &str) -> Result<bool, AppError> {
        let metadata = self.store.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT source_repository_path, branch_name, worktree_path,
                            base_commit, status, current_step, ownership_token,
                            ownership_claimed_at IS NOT NULL, git_metadata_json
                     FROM execution_runs WHERE run_id = ?1",
                    [run_id],
                    |row| {
                        let git_metadata_json: Option<String> = row.get(8)?;
                        let git_metadata = git_metadata_json
                            .as_deref()
                            .map(serde_json::from_str)
                            .transpose()
                            .map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    8,
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            })?;
                        Ok(ResumeMetadata {
                            source_repository_path: row.get(0)?,
                            branch_name: row.get(1)?,
                            worktree_path: PathBuf::from(row.get::<_, String>(2)?),
                            base_commit: row.get(3)?,
                            status: row.get(4)?,
                            current_step: row.get(5)?,
                            ownership_token: row.get(6)?,
                            ownership_claimed: row.get(7)?,
                            git_metadata,
                        })
                    },
                )
                .optional()?
                .ok_or_else(|| {
                    StoreError::App(AppError::not_found("The execution run could not be found."))
                })
        })?;
        if !matches!(metadata.status.as_str(), "blocked" | "failed") {
            return Err(AppError::conflict(
                "Only a blocked or failed execution can be resumed.",
            ));
        }
        if metadata.current_step == "complete" {
            return Err(AppError::conflict(
                "This execution run has already completed.",
            ));
        }
        if let Some(error) = self.run_lease_error(run_id)? {
            self.mark_blocked(run_id, &error.code, &error.message)?;
            return Ok(false);
        }
        if !metadata.worktree_path.exists() && !metadata.ownership_claimed {
            let result = preflight(
                &metadata.source_repository_path,
                &metadata.branch_name,
                &metadata.worktree_path,
            );
            if let Some(error) = result.error {
                self.mark_blocked(run_id, &error.code, &error.message)?;
                return Ok(false);
            }
            let timestamp = now();
            self.store.with_connection(|connection| {
                connection.execute(
                    "UPDATE execution_runs
                     SET base_commit = ?2, copilot_program = ?3,
                         verification_program = ?4, verification_args_json = ?5,
                         error_code = NULL, error_message = NULL, completed_at = NULL,
                         updated_at = ?6
                     WHERE run_id = ?1",
                    params![
                        run_id,
                        result.base_commit,
                        result.copilot_program,
                        result
                            .verification
                            .as_ref()
                            .map(|verification| verification.program.as_str()),
                        result
                            .verification
                            .as_ref()
                            .map(|verification| serde_json::to_string(&verification.arguments))
                            .transpose()
                            .map_err(|error| {
                                StoreError::App(AppError::database(format!(
                                    "Could not persist verification arguments: {error}"
                                )))
                            })?,
                        timestamp
                    ],
                )?;
                Ok(())
            })?;
            return Ok(true);
        }
        if let Some(error) = source_checkout_error(&metadata.source_repository_path) {
            self.mark_blocked(run_id, &error.code, &error.message)?;
            return Ok(false);
        }
        if metadata.ownership_claimed {
            if let Err(error) = self.validate_resume_ownership(run_id, &metadata) {
                self.mark_blocked(run_id, &error.code, &error.message)?;
                return Ok(false);
            }
        } else if metadata.worktree_path.exists() {
            self.mark_blocked(
                run_id,
                "worktree_ownership_conflict",
                "The expected managed worktree exists without a persisted Quorum ownership claim. Quorum left the path and branch unchanged.",
            )?;
            return Ok(false);
        }
        Ok(true)
    }

    fn run_lease_error(&self, run_id: &str) -> Result<Option<PreflightError>, AppError> {
        let path = self.store.run_lease_path(run_id)?;
        let lease = open_lock_file(&path).map_err(StoreError::from)?;
        match FileExt::try_lock_exclusive(&lease) {
            Ok(()) => {
                fs2::FileExt::unlock(&lease).map_err(StoreError::from)?;
                Ok(None)
            }
            Err(error) => Ok(Some(PreflightError {
                code: "orphan_process_active".to_owned(),
                message: format!(
                    "A process from the prior execution attempt still owns {} ({error}). Quorum will not resume or overlap it; wait for it to exit or terminate it explicitly, then retry.",
                    path.display(),
                ),
            })),
        }
    }

    fn validate_resume_ownership(
        &self,
        run_id: &str,
        metadata: &ResumeMetadata,
    ) -> Result<(), WorkerError> {
        let base_commit = metadata.base_commit.as_deref().ok_or_else(|| {
            WorkerError::new(
                "worktree_ownership_conflict",
                "The persisted ownership claim has no base commit.",
            )
        })?;
        let claim = OwnershipClaim {
            run_id: run_id.to_owned(),
            token: metadata.ownership_token.clone(),
            source_repository_path: metadata.source_repository_path.clone(),
            base_commit: base_commit.to_owned(),
            branch_name: metadata.branch_name.clone(),
            worktree_path: metadata.worktree_path.to_string_lossy().into_owned(),
        };
        self.ensure_resume_ownership_claim(&claim, &metadata.worktree_path)?;
        if metadata.worktree_path.exists() {
            let identity = if let Some(expected) = metadata.git_metadata.as_ref() {
                validate_owned_worktree(
                    &metadata.source_repository_path,
                    &metadata.branch_name,
                    &metadata.worktree_path,
                    Some(expected),
                )?
            } else {
                validate_incomplete_owned_worktree(
                    &metadata.source_repository_path,
                    &metadata.branch_name,
                    &metadata.worktree_path,
                    base_commit,
                )?
            };
            self.mark_ownership_verified(run_id, &identity)
                .map_err(WorkerError::database)?;
        } else {
            validate_owned_branch(
                &metadata.source_repository_path,
                &metadata.branch_name,
                base_commit,
            )?;
        }
        Ok(())
    }
}

struct ResumeMetadata {
    source_repository_path: String,
    branch_name: String,
    worktree_path: PathBuf,
    base_commit: Option<String>,
    status: String,
    current_step: String,
    ownership_token: String,
    ownership_claimed: bool,
    git_metadata: Option<GitMetadataIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct OwnershipClaim {
    run_id: String,
    token: String,
    source_repository_path: String,
    base_commit: String,
    branch_name: String,
    worktree_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct GitMetadataIdentity {
    git_dir: FilesystemIdentity,
    common_dir: FilesystemIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FilesystemIdentity {
    path: String,
    device: Option<u64>,
    inode: Option<u64>,
}

#[derive(Debug)]
struct StartTarget {
    work_item_id: String,
    plan_id: String,
    title: String,
    repository_path: String,
    builder_model: String,
    reviewer_model: String,
}

#[derive(Debug)]
struct VerificationCommand {
    program: String,
    arguments: Vec<String>,
}

#[derive(Debug)]
struct PreflightError {
    code: String,
    message: String,
}

#[derive(Debug)]
struct Preflight {
    base_commit: Option<String>,
    copilot_program: Option<String>,
    verification: Option<VerificationCommand>,
    error: Option<PreflightError>,
}

#[allow(clippy::too_many_lines)]
fn preflight(repository_path: &str, branch_name: &str, worktree_path: &Path) -> Preflight {
    let git = resolve_executable("git");
    let copilot = resolve_executable("copilot");
    preflight_with_executables(
        repository_path,
        branch_name,
        worktree_path,
        git.as_deref(),
        copilot.as_deref(),
    )
}

#[allow(clippy::too_many_lines)]
fn preflight_with_executables(
    repository_path: &str,
    branch_name: &str,
    worktree_path: &Path,
    git: Option<&str>,
    copilot: Option<&str>,
) -> Preflight {
    let mut diagnostics = Vec::new();
    if let Some(error) = confinement_error() {
        diagnostics.push(("sandbox_unavailable", error));
    }
    if git.is_none() {
        diagnostics.push((
            "missing_git",
            "Git executable `git` was not found on PATH. Install Git and retry.".to_owned(),
        ));
    }
    if copilot.is_none() {
        diagnostics.push((
            "missing_copilot",
            "GitHub Copilot CLI executable `copilot` was not found on PATH. Install it and retry."
                .to_owned(),
        ));
    }

    let mut base_commit = None;
    if let Some(git) = git {
        let root = direct_output(git, repository_path, &["rev-parse", "--show-toplevel"]);
        match root {
            Ok(output) if output.status.success() => {
                let actual_root = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                let expected = fs::canonicalize(repository_path);
                let actual = fs::canonicalize(&actual_root);
                if expected.is_err() || actual.is_err() || expected.ok() != actual.ok() {
                    diagnostics.push((
                        "repository_mismatch",
                        format!(
                            "The registered path no longer identifies its original Git root: {repository_path}."
                        ),
                    ));
                }
            }

            Ok(output) => diagnostics.push((
                "invalid_repository",
                command_diagnostic(
                    "Git could not inspect the registered repository root.",
                    &output.stderr,
                ),
            )),
            Err(error) => diagnostics.push((
                "invalid_repository",
                format!("Git could not inspect the registered repository: {error}"),
            )),
        }
        let attached = direct_output(git, repository_path, &["symbolic-ref", "--quiet", "HEAD"]);
        if !matches!(attached, Ok(ref output) if output.status.success()) {
            diagnostics.push((
                "detached_head",
                "Execution requires the source checkout to be on an attached branch. Check out the intended base branch and retry.".to_owned(),
            ));
        }
        match direct_output(git, repository_path, &["rev-parse", "HEAD"]) {
            Ok(output) if output.status.success() => {
                base_commit = Some(String::from_utf8_lossy(&output.stdout).trim().to_owned());
            }
            Ok(output) => diagnostics.push((
                "missing_head",
                command_diagnostic("Execution requires a valid committed HEAD.", &output.stderr),
            )),
            Err(error) => diagnostics.push((
                "missing_head",
                format!("Git could not resolve the source HEAD: {error}"),
            )),
        }
        match direct_output(
            git,
            repository_path,
            &["status", "--porcelain=v1", "--untracked-files=normal"],
        ) {
            Ok(output) if output.status.success() && output.stdout.is_empty() => {}
            Ok(output) if output.status.success() => diagnostics.push((
                "dirty_source",
                "Execution requires a clean source checkout. Commit or otherwise handle the listed tracked or untracked changes yourself; Quorum will never reset, stash, or clean them."
                    .to_owned(),
            )),
            Ok(output) => diagnostics.push((
                "git_status_failed",
                command_diagnostic("Git could not verify the source checkout.", &output.stderr),
            )),
            Err(error) => diagnostics.push((
                "git_status_failed",
                format!("Git could not verify the source checkout: {error}"),
            )),
        }
        if matches!(
            direct_output(
                git,
                repository_path,
                &["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch_name}")],
            ),
            Ok(ref output) if output.status.success()
        ) {
            diagnostics.push((
                "branch_conflict",
                format!(
                    "The managed branch {branch_name} already exists. Quorum left it unchanged."
                ),
            ));
        }
    }

    if worktree_path.exists() {
        diagnostics.push((
            "worktree_conflict",
            format!(
                "The managed worktree path {} already exists. Quorum left it unchanged.",
                worktree_path.display()
            ),
        ));
    }
    if let (Ok(source), Ok(app_data)) = (
        fs::canonicalize(repository_path),
        fs::canonicalize(
            worktree_path
                .parent()
                .and_then(Path::parent)
                .unwrap_or(worktree_path),
        ),
    ) {
        if app_data.starts_with(source) {
            diagnostics.push((
                "unsafe_worktree_location",
                "Quorum's application-data worktree directory is inside the registered repository. Move the application data or register a different checkout."
                    .to_owned(),
            ));
        }
    }

    let verification = match discover_verification(Path::new(repository_path)) {
        Ok(verification) => Some(verification),
        Err(error) => {
            diagnostics.push(("verification_unavailable", error));
            None
        }
    };
    let error = diagnostics.first().map(|(code, _)| PreflightError {
        code: (*code).to_owned(),
        message: diagnostics
            .iter()
            .map(|(_, message)| message.as_str())
            .collect::<Vec<_>>()
            .join("\n\n"),
    });
    Preflight {
        base_commit,
        copilot_program: copilot.map(str::to_owned),
        verification,
        error,
    }
}

#[cfg(target_os = "macos")]
fn confinement_error() -> Option<String> {
    (!Path::new("/usr/bin/sandbox-exec").is_file()).then(|| {
        "Quorum requires macOS sandbox-exec to confine the entire Copilot CLI process to the managed worktree. The local Copilot sandbox only confines shell commands and cannot protect against built-in file edits."
            .to_owned()
    })
}

#[cfg(not(target_os = "macos"))]
fn confinement_error() -> Option<String> {
    Some(
        "Quorum execution is fail-closed on this platform because no whole-process Copilot confinement backend is configured."
            .to_owned(),
    )
}

fn source_checkout_error(repository_path: &str) -> Option<PreflightError> {
    if let Some(message) = confinement_error() {
        return Some(PreflightError {
            code: "sandbox_unavailable".to_owned(),
            message,
        });
    }
    let git = resolve_executable("git").ok_or_else(|| PreflightError {
        code: "missing_git".to_owned(),
        message: "Git executable `git` was not found on PATH. Install Git and retry.".to_owned(),
    });
    let git = match git {
        Ok(git) => git,
        Err(error) => return Some(error),
    };
    let root = direct_output(&git, repository_path, &["rev-parse", "--show-toplevel"]);
    match root {
        Ok(output) if output.status.success() => {
            let actual_root = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if fs::canonicalize(repository_path).ok() != fs::canonicalize(actual_root).ok() {
                return Some(PreflightError {
                    code: "repository_mismatch".to_owned(),
                    message: format!(
                        "The registered path no longer identifies its original Git root: {repository_path}."
                    ),
                });
            }
        }
        Ok(output) => {
            return Some(PreflightError {
                code: "invalid_repository".to_owned(),
                message: command_diagnostic(
                    "Git could not inspect the registered repository root.",
                    &output.stderr,
                ),
            });
        }
        Err(error) => {
            return Some(PreflightError {
                code: "invalid_repository".to_owned(),
                message: format!("Git could not inspect the registered repository: {error}"),
            });
        }
    }
    if !matches!(
        direct_output(&git, repository_path, &["symbolic-ref", "--quiet", "HEAD"]),
        Ok(ref output) if output.status.success()
    ) {
        return Some(PreflightError {
            code: "detached_head".to_owned(),
            message: "Execution resume requires the source checkout to remain on an attached branch. Check out a branch and retry.".to_owned(),
        });
    }
    if !matches!(
        direct_output(&git, repository_path, &["rev-parse", "HEAD"]),
        Ok(ref output) if output.status.success()
    ) {
        return Some(PreflightError {
            code: "missing_head".to_owned(),
            message: "Execution resume requires a valid committed source HEAD.".to_owned(),
        });
    }
    match direct_output(
        &git,
        repository_path,
        &["status", "--porcelain=v1", "--untracked-files=normal"],
    ) {
        Ok(output) if output.status.success() && output.stdout.is_empty() => None,
        Ok(output) if output.status.success() => Some(PreflightError {
            code: "dirty_source".to_owned(),
            message: "Execution resume requires a clean source checkout. Commit or otherwise handle tracked or untracked changes yourself; Quorum will never reset, stash, or clean them."
                .to_owned(),
        }),
        Ok(output) => Some(PreflightError {
            code: "git_status_failed".to_owned(),
            message: command_diagnostic(
                "Git could not verify the source checkout before resume.",
                &output.stderr,
            ),
        }),
        Err(error) => Some(PreflightError {
            code: "git_status_failed".to_owned(),
            message: format!("Git could not verify the source checkout before resume: {error}"),
        }),
    }
}

fn discover_verification(repository_path: &Path) -> Result<VerificationCommand, String> {
    let makefile = repository_path.join("Makefile");
    if makefile.is_file() {
        let contents = fs::read_to_string(&makefile).map_err(|error| {
            format!(
                "Quorum could not read {} while discovering verification: {error}",
                makefile.display()
            )
        })?;
        for target in ["check", "test"] {
            if makefile_has_target(&contents, target) {
                return executable_command("make", vec![target.to_owned()]);
            }
        }
    }
    let package = repository_path.join("package.json");
    if package.is_file() {
        let contents = fs::read_to_string(&package).map_err(|error| {
            format!(
                "Quorum could not read {} while discovering verification: {error}",
                package.display()
            )
        })?;
        let value: Value = serde_json::from_str(&contents).map_err(|error| {
            format!(
                "Quorum could not parse {} while discovering verification: {error}",
                package.display()
            )
        })?;
        if value
            .get("scripts")
            .and_then(|scripts| scripts.get("test"))
            .and_then(Value::as_str)
            .is_some_and(|script| !script.trim().is_empty())
        {
            return executable_command("npm", vec!["test".to_owned()]);
        }
    }
    if repository_path.join("Cargo.toml").is_file() {
        return executable_command("cargo", vec!["test".to_owned()]);
    }
    Err(
        "No verification command was discovered. Add a Makefile `check` or `test` target, a package.json `test` script, or a Cargo.toml before retrying."
            .to_owned(),
    )
}

fn executable_command(
    executable: &str,
    arguments: Vec<String>,
) -> Result<VerificationCommand, String> {
    resolve_executable(executable)
        .map(|program| VerificationCommand { program, arguments })
        .ok_or_else(|| {
            format!(
                "Verification selected `{executable}`, but that executable was not found on PATH."
            )
        })
}

fn makefile_has_target(contents: &str, target: &str) -> bool {
    contents.lines().any(|line| {
        let line = line.split('#').next().unwrap_or_default();
        if line.starts_with(char::is_whitespace) {
            return false;
        }
        line.split_once(':').is_some_and(|(targets, _)| {
            targets
                .split_whitespace()
                .any(|candidate| candidate == target)
        })
    })
}

fn resolve_executable(program: &str) -> Option<String> {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return executable_file(path).then(|| path.to_string_lossy().into_owned());
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|directory| directory.join(program))
        .find(|candidate| executable_file(candidate))
        .map(|candidate| candidate.to_string_lossy().into_owned())
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn direct_output(program: &str, cwd: &str, arguments: &[&str]) -> io::Result<std::process::Output> {
    trusted_direct_git_command(program, cwd)
        .args(arguments)
        .stdin(Stdio::null())
        .output()
}

fn direct_output_with_input(
    program: &str,
    cwd: &str,
    arguments: &[&str],
    input: Vec<u8>,
) -> io::Result<std::process::Output> {
    let mut child = trusted_direct_git_command(program, cwd)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("could not open trusted Git stdin"))?;
    let writer = thread::spawn(move || stdin.write_all(&input));
    let output = child.wait_with_output();
    writer
        .join()
        .map_err(|_| io::Error::other("trusted Git stdin writer panicked"))??;
    output
}

fn trusted_direct_git_command(program: &str, cwd: &str) -> Command {
    let mut command = Command::new(program);
    clear_git_environment(&mut command);
    command
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
        ])
        .env("GIT_CONFIG", "/dev/null")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(cwd);
    command
}

fn clear_git_environment(command: &mut Command) {
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
}

fn trusted_provisioning_git_request(
    git: String,
    arguments: Vec<String>,
    cwd: PathBuf,
    disabled_hooks: &Path,
) -> ProcessRequest {
    let mut trusted_arguments = vec![
        "-c".to_owned(),
        format!("core.hooksPath={}", disabled_hooks.display()),
        "-c".to_owned(),
        "core.fsmonitor=false".to_owned(),
    ];
    trusted_arguments.extend(arguments);
    let mut request = ProcessRequest::new(git, trusted_arguments, cwd);
    request.clear_git_environment = true;
    request.environment = vec![
        ("GIT_CONFIG".to_owned(), "/dev/null".to_owned()),
        ("GIT_CONFIG_GLOBAL".to_owned(), "/dev/null".to_owned()),
        ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
        ("GIT_OPTIONAL_LOCKS".to_owned(), "0".to_owned()),
        ("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned()),
    ];
    request
}

fn reject_executable_checkout_filters(git: &str, repository_path: &str) -> Result<(), WorkerError> {
    reject_configured_filter_commands(git, repository_path)?;
    reject_filter_attributes(git, repository_path)
}

fn reject_configured_filter_commands(git: &str, repository_path: &str) -> Result<(), WorkerError> {
    let configured = repository_config_output(
        git,
        repository_path,
        &[
            "config",
            "--includes",
            "--null",
            "--name-only",
            "--get-regexp",
            r"^filter\..*\.(smudge|process)$",
        ],
    )
    .map_err(|error| {
        WorkerError::new(
            "unsafe_git_configuration",
            format!("Git could not inspect configured checkout filters: {error}"),
        )
    })?;
    match configured.status.code() {
        Some(1) => {}
        Some(0) => {
            let filters = configured
                .stdout
                .split(|byte| *byte == 0)
                .filter(|name| !name.is_empty())
                .map(|name| String::from_utf8_lossy(name).into_owned())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(WorkerError::new(
                "unsafe_checkout_filter",
                format!(
                    "Quorum will not provision a worktree while repository config defines executable checkout filters ({filters}). Disable the filter process/smudge commands or materialize the repository without them, then retry."
                ),
            ));
        }
        _ => {
            return Err(WorkerError::new(
                "unsafe_git_configuration",
                command_diagnostic(
                    "Git could not inspect configured checkout filters.",
                    &configured.stderr,
                ),
            ));
        }
    }
    Ok(())
}

fn repository_config_output(
    git: &str,
    repository_path: &str,
    arguments: &[&str],
) -> io::Result<std::process::Output> {
    let mut command = Command::new(git);
    clear_git_environment(&mut command);
    command
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
        ])
        .args(arguments)
        .current_dir(repository_path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
}

fn reject_filter_attributes(git: &str, repository_path: &str) -> Result<(), WorkerError> {
    let tracked = direct_output(git, repository_path, &["ls-files", "-z"]).map_err(|error| {
        WorkerError::new(
            "unsafe_git_configuration",
            format!("Git could not enumerate paths for checkout-filter inspection: {error}"),
        )
    })?;
    if !tracked.status.success() {
        return Err(WorkerError::new(
            "unsafe_git_configuration",
            command_diagnostic(
                "Git could not enumerate paths for checkout-filter inspection.",
                &tracked.stderr,
            ),
        ));
    }
    let attributes = direct_output_with_input(
        git,
        repository_path,
        &["check-attr", "--cached", "-z", "--stdin", "filter"],
        tracked.stdout,
    )
    .map_err(|error| {
        WorkerError::new(
            "unsafe_git_configuration",
            format!("Git could not inspect checkout-filter attributes: {error}"),
        )
    })?;
    if !attributes.status.success() {
        return Err(WorkerError::new(
            "unsafe_git_configuration",
            command_diagnostic(
                "Git could not inspect checkout-filter attributes.",
                &attributes.stderr,
            ),
        ));
    }
    let mut fields = attributes.stdout.split(|byte| *byte == 0).peekable();
    let mut filtered_paths = Vec::new();
    while fields.peek().is_some_and(|field| !field.is_empty()) {
        let path = fields.next().expect("peeked path");
        let attribute = fields.next().ok_or_else(|| {
            WorkerError::new(
                "unsafe_git_configuration",
                "Git returned malformed checkout-filter attribute evidence.",
            )
        })?;
        let value = fields.next().ok_or_else(|| {
            WorkerError::new(
                "unsafe_git_configuration",
                "Git returned malformed checkout-filter attribute evidence.",
            )
        })?;
        if attribute != b"filter" {
            return Err(WorkerError::new(
                "unsafe_git_configuration",
                "Git returned unexpected checkout-filter attribute evidence.",
            ));
        }
        if value != b"unspecified" && value != b"unset" && filtered_paths.len() < 5 {
            filtered_paths.push(String::from_utf8_lossy(path).into_owned());
        }
    }
    if filtered_paths.is_empty() {
        Ok(())
    } else {
        Err(WorkerError::new(
            "unsafe_checkout_filter",
            format!(
                "Quorum will not provision a worktree whose tracked files require checkout filters ({}). Remove the filter attributes or materialize those files safely before retrying.",
                filtered_paths.join(", ")
            ),
        ))
    }
}

fn command_diagnostic(prefix: &str, stderr: &[u8]) -> String {
    let detail = String::from_utf8_lossy(stderr);
    if detail.trim().is_empty() {
        prefix.to_owned()
    } else {
        format!("{prefix} {}", detail.trim())
    }
}

fn required(value: &str, message: &str) -> Result<String, AppError> {
    let value = value.trim();
    if value.is_empty() {
        Err(AppError::validation(message))
    } else {
        Ok(value.to_owned())
    }
}

fn readable_slug(value: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            separator = false;
            slug.push(character);
            if slug.len() >= 40 {
                break;
            }
        } else {
            separator = true;
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "work-item".to_owned()
    } else {
        slug.to_owned()
    }
}

fn short_id(value: &str) -> String {
    value
        .chars()
        .filter(|character| *character != '-')
        .take(8)
        .collect()
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn rotate_unconfirmed_session(
    transaction: &Transaction<'_>,
    run_id: &str,
    current_step: &str,
    timestamp: &str,
) -> Result<(), StoreError> {
    let (state_column, id_column, name_column) = match current_step {
        "building" | "remediating" => (
            "builder_session_state",
            "builder_session_id",
            "builder_session_name",
        ),
        "reviewing" => (
            "reviewer_session_state",
            "reviewer_session_id",
            "reviewer_session_name",
        ),
        _ => return Ok(()),
    };
    let query =
        format!("SELECT {state_column}, {name_column} FROM execution_runs WHERE run_id = ?1");
    let (state, name): (String, String) =
        transaction.query_row(&query, [run_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    if state != "not_started" {
        return Ok(());
    }
    let session_id = Uuid::new_v4().to_string();
    let base_name = name.split("-retry-").next().unwrap_or(&name);
    let session_name = format!("{base_name}-retry-{}", short_id(&session_id));
    let update = format!(
        "UPDATE execution_runs
         SET {id_column} = ?2, {name_column} = ?3, updated_at = ?4
         WHERE run_id = ?1 AND {state_column} = 'not_started'"
    );
    transaction.execute(
        &update,
        params![run_id, session_id, session_name, timestamp],
    )?;
    Ok(())
}

fn copilot_log_messages(stdout: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|event| {
            let event_type = event.get("type").and_then(Value::as_str)?;
            match event_type {
                "assistant.message" => {
                    let content = event
                        .get("data")
                        .and_then(|data| data.get("content"))
                        .or_else(|| event.get("content"))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|content| !content.is_empty());
                    Some(content.map_or_else(
                        || "Copilot completed the session.".to_owned(),
                        ToOwned::to_owned,
                    ))
                }
                "error" => event
                    .get("message")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                _ => None,
            }
        })
        .collect()
}

fn status_for_step(step: &str) -> &'static str {
    match step {
        "preparing" => "starting",
        "building" => "building",
        "verifying" => "verifying",
        "reviewing" => "reviewing",
        "remediating" => "remediating",
        _ => "blocked",
    }
}

fn phase_for_step(step: &str) -> &'static str {
    match step {
        "reviewing" => "reviewing",
        "complete" => "delivery",
        _ => "building",
    }
}

fn resume_allowed(
    error_code: Option<&str>,
    iteration: usize,
    max_iterations: usize,
    blocking_findings: usize,
) -> bool {
    match error_code {
        Some("blocking_findings") => blocking_findings == 0,
        Some("verification_failed") => iteration < max_iterations,
        _ => true,
    }
}

fn append_event(
    transaction: &Transaction<'_>,
    run_id: &str,
    event_kind: &str,
    payload: &Value,
    timestamp: &str,
) -> Result<(), StoreError> {
    let sequence: usize = transaction.query_row(
        "SELECT COALESCE(MAX(sequence), -1) + 1 FROM phase_events WHERE run_id = ?1",
        [run_id],
        |row| row.get(0),
    )?;
    transaction.execute(
        "INSERT INTO phase_events (
           id, run_id, sequence, event_kind, payload_json, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            Uuid::new_v4().to_string(),
            run_id,
            sequence,
            event_kind,
            payload.to_string(),
            timestamp
        ],
    )?;
    Ok(())
}

fn blocking_findings(connection: &Connection, run_id: &str) -> Result<usize, rusqlite::Error> {
    connection.query_row(
        "SELECT count(*) FROM execution_findings
         WHERE run_id = ?1 AND severity = 'blocking' AND status = 'open'",
        [run_id],
        |row| row.get(0),
    )
}

fn mark_ready(
    transaction: &Transaction<'_>,
    run_id: &str,
    expected_state_digest: &str,
    timestamp: &str,
) -> Result<(), StoreError> {
    let state: (Option<String>, Option<String>, usize) = transaction.query_row(
        "SELECT verified_state_digest, reviewed_state_digest,
                (SELECT count(*) FROM execution_findings
                 WHERE run_id = ?1 AND severity = 'blocking' AND status = 'open')
         FROM execution_runs WHERE run_id = ?1",
        [run_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if state.0.as_deref() != Some(expected_state_digest)
        || state.1.as_deref() != Some(expected_state_digest)
        || state.2 != 0
    {
        return Err(StoreError::App(AppError::conflict(
            "Delivery readiness requires unchanged, matching verified and reviewed full-state digests with no open blocking findings.",
        )));
    }
    let changed = transaction.execute(
        "UPDATE execution_runs
         SET status = 'ready', current_step = 'complete',
             error_code = NULL, error_message = NULL,
             updated_at = ?2, completed_at = ?2
         WHERE run_id = ?1 AND status IN ('reviewing', 'blocked')",
        params![run_id, timestamp],
    )?;
    if changed != 1 {
        return Err(StoreError::App(AppError::conflict(
            "The execution run is no longer eligible for delivery readiness.",
        )));
    }
    transaction.execute(
        "UPDATE runs
         SET phase = 'delivery', outcome = 'succeeded', updated_at = ?2
         WHERE id = ?1",
        params![run_id, timestamp],
    )?;
    transaction.execute(
        "UPDATE execution_attempts
         SET status = 'succeeded', completed_at = ?2
         WHERE run_id = ?1 AND status = 'running'",
        params![run_id, timestamp],
    )?;
    append_event(
        transaction,
        run_id,
        "delivery_ready",
        &json!({"message": "Verification passed and no blocking review findings remain."}),
        timestamp,
    )
}

#[allow(clippy::too_many_lines)]
fn load_detail(connection: &Connection, run_id: &str) -> Result<ExecutionDetailDto, StoreError> {
    let run = connection
        .query_row(
            "SELECT runs.id, runs.work_item_id, runs.plan_id, execution_runs.queue_entry_id,
                    runs.phase, runs.outcome, execution_runs.status,
                    execution_runs.current_step, execution_runs.base_commit,
                    execution_runs.branch_name, execution_runs.worktree_path,
                    execution_runs.builder_session_name, execution_runs.builder_model,
                    execution_runs.reviewer_session_name, execution_runs.reviewer_model,
                    execution_runs.verification_program,
                    execution_runs.verification_args_json,
                    execution_runs.iteration, execution_runs.max_iterations,
                    execution_runs.error_code, execution_runs.error_message,
                    execution_runs.created_at, execution_runs.updated_at,
                    execution_runs.completed_at
             FROM execution_runs
             JOIN runs ON runs.id = execution_runs.run_id
             WHERE execution_runs.run_id = ?1",
            [run_id],
            |row| {
                let arguments_json: Option<String> = row.get(16)?;
                let verification_arguments = arguments_json
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()
                    .map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            16,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?
                    .unwrap_or_default();
                Ok(ExecutionRunDto {
                    id: row.get(0)?,
                    work_item_id: row.get(1)?,
                    plan_id: row.get(2)?,
                    queue_entry_id: row.get(3)?,
                    phase: row.get(4)?,
                    outcome: row
                        .get::<_, Option<String>>(5)?
                        .unwrap_or_else(|| "pending".to_owned()),
                    status: row.get(6)?,
                    current_step: row.get(7)?,
                    base_commit: row.get(8)?,
                    branch_name: row.get(9)?,
                    worktree_path: row.get(10)?,
                    builder_session_name: row.get(11)?,
                    builder_model: row.get(12)?,
                    reviewer_session_name: row.get(13)?,
                    reviewer_model: row.get(14)?,
                    verification_program: row.get(15)?,
                    verification_arguments,
                    iteration: row.get(17)?,
                    max_iterations: row.get(18)?,
                    error_code: row.get(19)?,
                    error_message: row.get(20)?,
                    created_at: row.get(21)?,
                    updated_at: row.get(22)?,
                    completed_at: row.get(23)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| {
            StoreError::App(AppError::not_found("The execution run could not be found."))
        })?;
    let mut attempts_statement = connection.prepare(
        "SELECT id, number, reason, status, error_code, error_message,
                started_at, completed_at
         FROM execution_attempts WHERE run_id = ?1 ORDER BY number",
    )?;
    let attempts = attempts_statement
        .query_map([run_id], |row| {
            Ok(ExecutionAttemptDto {
                id: row.get(0)?,
                number: row.get(1)?,
                reason: row.get(2)?,
                status: row.get(3)?,
                error_code: row.get(4)?,
                error_message: row.get(5)?,
                started_at: row.get(6)?,
                completed_at: row.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut logs_statement = connection.prepare(
        "SELECT execution_logs.id, execution_logs.command_id,
                execution_commands.phase, execution_commands.program,
                execution_logs.stream, execution_logs.sequence, execution_logs.text,
                execution_logs.truncated, execution_logs.created_at
         FROM execution_logs
         JOIN execution_commands ON execution_commands.id = execution_logs.command_id
         WHERE execution_logs.run_id = ?1
         ORDER BY execution_logs.created_at DESC, execution_logs.sequence DESC LIMIT 80",
    )?;
    let mut recent_logs = logs_statement
        .query_map([run_id], |row| {
            Ok(ExecutionLogDto {
                id: row.get(0)?,
                command_id: row.get(1)?,
                phase: row.get(2)?,
                program: row.get(3)?,
                stream: row.get(4)?,
                sequence: row.get(5)?,
                text: row.get(6)?,
                truncated: row.get(7)?,
                created_at: row.get(8)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    recent_logs.reverse();
    let mut findings_statement = connection.prepare(
        "SELECT id, external_id, severity, title, body, path, line, status,
                disposition_note, first_seen_iteration, last_seen_iteration,
                created_at, updated_at, resolved_at
         FROM execution_findings WHERE run_id = ?1
         ORDER BY CASE severity WHEN 'blocking' THEN 0 ELSE 1 END,
                  first_seen_iteration, external_id",
    )?;
    let findings = findings_statement
        .query_map([run_id], |row| {
            Ok(ExecutionFindingDto {
                id: row.get(0)?,
                external_id: row.get(1)?,
                severity: row.get(2)?,
                title: row.get(3)?,
                body: row.get(4)?,
                path: row.get(5)?,
                line: row.get(6)?,
                status: row.get(7)?,
                disposition_note: row.get(8)?,
                first_seen_iteration: row.get(9)?,
                last_seen_iteration: row.get(10)?,
                created_at: row.get(11)?,
                updated_at: row.get(12)?,
                resolved_at: row.get(13)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut events_statement = connection.prepare(
        "SELECT id, sequence, event_kind, payload_json, created_at
         FROM phase_events WHERE run_id = ?1 ORDER BY sequence DESC LIMIT 40",
    )?;
    let mut recent_events = events_statement
        .query_map([run_id], |row| {
            let payload_json: String = row.get(3)?;
            let payload = serde_json::from_str(&payload_json).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            Ok(ExecutionPhaseEventDto {
                id: row.get(0)?,
                sequence: row.get(1)?,
                event_kind: row.get(2)?,
                payload,
                created_at: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    recent_events.reverse();
    let blocking_finding_count = findings
        .iter()
        .filter(|finding| finding.severity == "blocking" && finding.status == "open")
        .count();
    let can_cancel = matches!(
        run.status.as_str(),
        "starting" | "building" | "verifying" | "reviewing" | "remediating"
    );
    let can_resume = matches!(run.status.as_str(), "blocked" | "failed")
        && run.current_step != "complete"
        && resume_allowed(
            run.error_code.as_deref(),
            run.iteration,
            run.max_iterations,
            blocking_finding_count,
        );
    let delivery_ready = run.status == "ready" && blocking_finding_count == 0;
    Ok(ExecutionDetailDto {
        run,
        attempts,
        recent_logs,
        findings,
        recent_events,
        blocking_finding_count,
        can_resume,
        can_cancel,
        delivery_ready,
    })
}

#[derive(Debug)]
struct WorkerSnapshot {
    run_id: String,
    plan_markdown: String,
    acceptance_intent: String,
    source_repository_path: String,
    base_commit: String,
    branch_name: String,
    worktree_path: PathBuf,
    ownership_token: String,
    ownership_claimed: bool,
    git_metadata: Option<GitMetadataIdentity>,
    copilot_program: String,
    builder_session_id: String,
    builder_session_name: String,
    builder_model: String,
    reviewer_session_id: String,
    reviewer_session_name: String,
    reviewer_model: String,
    verification_program: String,
    verification_arguments: Vec<String>,
    current_step: String,
    iteration: usize,
    max_iterations: usize,
    builder_session_state: String,
    reviewer_session_state: String,
    pending_builder_prompt: Option<String>,
    remediation_diff_hash: Option<String>,
    verified_state_digest: Option<String>,
    reviewed_state_digest: Option<String>,
}

#[derive(Debug)]
struct WorkerError {
    code: String,
    message: String,
}

impl WorkerError {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    fn database(error: AppError) -> Self {
        Self::new("database", error.message)
    }
}

struct CommandExecution {
    id: String,
    result: ProcessResult,
}

#[derive(Debug)]
struct BaseEvidence {
    digest: String,
    review_diff: String,
}

impl ExecutionService {
    fn run_worker(&self, run_id: &str, control: &RunControl) {
        if let Err(error) = self.worker_loop(run_id, control) {
            if control.cancelled() {
                let _ = self.mark_cancelled(run_id);
            } else {
                let _ = self.mark_blocked(run_id, &error.code, &error.message);
            }
        }
    }

    fn worker_loop(&self, run_id: &str, control: &RunControl) -> Result<(), WorkerError> {
        loop {
            if control.cancelled() {
                self.mark_cancelled(run_id).map_err(WorkerError::database)?;
                return Ok(());
            }
            let snapshot = self.snapshot(run_id).map_err(WorkerError::database)?;
            match snapshot.current_step.as_str() {
                "preparing" => {
                    self.prepare_worktree(&snapshot, control)?;
                    self.transition(
                        run_id,
                        "building",
                        "building",
                        "building",
                        "worktree_ready",
                        &json!({
                            "branchName": snapshot.branch_name,
                            "worktreePath": snapshot.worktree_path,
                            "baseCommit": snapshot.base_commit,
                        }),
                    )
                    .map_err(WorkerError::database)?;
                }
                "building" | "remediating" => {
                    self.run_builder(&snapshot, control)?;
                }
                "verifying" => {
                    self.run_verification(&snapshot, control)?;
                }
                "reviewing" => {
                    self.run_review(&snapshot, control)?;
                }
                "complete" => return Ok(()),
                step => {
                    return Err(WorkerError::new(
                        "invalid_execution_state",
                        format!("Execution contains an unsupported step `{step}`."),
                    ));
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn snapshot(&self, run_id: &str) -> Result<WorkerSnapshot, AppError> {
        self.store.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT execution_runs.run_id, plans.markdown_body,
                            work_items.markdown_body,
                            execution_runs.source_repository_path,
                            execution_runs.base_commit, execution_runs.branch_name,
                            execution_runs.worktree_path, execution_runs.ownership_token,
                            execution_runs.ownership_claimed_at IS NOT NULL,
                            execution_runs.git_metadata_json,
                            execution_runs.copilot_program,
                            execution_runs.builder_session_id,
                            execution_runs.builder_session_name,
                            execution_runs.builder_model,
                            execution_runs.reviewer_session_id,
                            execution_runs.reviewer_session_name,
                            execution_runs.reviewer_model,
                            execution_runs.verification_program,
                            execution_runs.verification_args_json,
                            execution_runs.current_step,
                            execution_runs.iteration, execution_runs.max_iterations,
                            execution_runs.builder_session_state,
                            execution_runs.reviewer_session_state,
                            execution_runs.pending_builder_prompt,
                            execution_runs.remediation_diff_hash,
                            execution_runs.verified_state_digest,
                            execution_runs.reviewed_state_digest
                     FROM execution_runs
                     JOIN runs ON runs.id = execution_runs.run_id
                     JOIN plans ON plans.id = runs.plan_id
                     JOIN work_items ON work_items.id = runs.work_item_id
                     WHERE execution_runs.run_id = ?1",
                    [run_id],
                    |row| {
                        let git_metadata_json: Option<String> = row.get(9)?;
                        let git_metadata = git_metadata_json
                            .as_deref()
                            .map(serde_json::from_str)
                            .transpose()
                            .map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    9,
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            })?;
                        let arguments_json: String = row.get(18)?;
                        let verification_arguments = serde_json::from_str(&arguments_json)
                            .map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    18,
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            })?;
                        Ok(WorkerSnapshot {
                            run_id: row.get(0)?,
                            plan_markdown: row.get(1)?,
                            acceptance_intent: row.get(2)?,
                            source_repository_path: row.get(3)?,
                            base_commit: row.get::<_, Option<String>>(4)?.ok_or_else(|| {
                                rusqlite::Error::InvalidColumnType(
                                    4,
                                    "base_commit".to_owned(),
                                    rusqlite::types::Type::Null,
                                )
                            })?,
                            branch_name: row.get(5)?,
                            worktree_path: PathBuf::from(row.get::<_, String>(6)?),
                            ownership_token: row.get(7)?,
                            ownership_claimed: row.get(8)?,
                            git_metadata,
                            copilot_program: row.get::<_, Option<String>>(10)?.ok_or_else(
                                || {
                                    rusqlite::Error::InvalidColumnType(
                                        10,
                                        "copilot_program".to_owned(),
                                        rusqlite::types::Type::Null,
                                    )
                                },
                            )?,
                            builder_session_id: row.get(11)?,
                            builder_session_name: row.get(12)?,
                            builder_model: row.get(13)?,
                            reviewer_session_id: row.get(14)?,
                            reviewer_session_name: row.get(15)?,
                            reviewer_model: row.get(16)?,
                            verification_program: row.get::<_, Option<String>>(17)?.ok_or_else(
                                || {
                                    rusqlite::Error::InvalidColumnType(
                                        17,
                                        "verification_program".to_owned(),
                                        rusqlite::types::Type::Null,
                                    )
                                },
                            )?,
                            verification_arguments,
                            current_step: row.get(19)?,
                            iteration: row.get(20)?,
                            max_iterations: row.get(21)?,
                            builder_session_state: row.get(22)?,
                            reviewer_session_state: row.get(23)?,
                            pending_builder_prompt: row.get(24)?,
                            remediation_diff_hash: row.get(25)?,
                            verified_state_digest: row.get(26)?,
                            reviewed_state_digest: row.get(27)?,
                        })
                    },
                )
                .optional()?
                .ok_or_else(|| {
                    StoreError::App(AppError::not_found("The execution run could not be found."))
                })
        })
    }

    #[allow(clippy::too_many_lines)]
    fn prepare_worktree(
        &self,
        snapshot: &WorkerSnapshot,
        control: &RunControl,
    ) -> Result<(), WorkerError> {
        if let Some(error) = source_checkout_error(&snapshot.source_repository_path) {
            return Err(WorkerError::new(error.code, error.message));
        }
        let parent = snapshot.worktree_path.parent().ok_or_else(|| {
            WorkerError::new(
                "unsafe_worktree_path",
                "The managed worktree path has no parent directory.",
            )
        })?;
        fs::create_dir_all(parent).map_err(|error| {
            WorkerError::new(
                "worktree_path",
                format!(
                    "Quorum could not create the managed worktree directory {}: {error}",
                    parent.display()
                ),
            )
        })?;
        let git = resolve_executable("git").ok_or_else(|| {
            WorkerError::new(
                "missing_git",
                "Git executable `git` is no longer available on PATH.",
            )
        })?;
        let disabled_hooks = self.disabled_git_hooks_directory(&snapshot.run_id)?;
        let branch = self.execute_command(
            snapshot,
            control,
            "preparing",
            trusted_provisioning_git_request(
                git.clone(),
                vec![
                    "-C".to_owned(),
                    snapshot.source_repository_path.clone(),
                    "show-ref".to_owned(),
                    "--verify".to_owned(),
                    "--quiet".to_owned(),
                    format!("refs/heads/{}", snapshot.branch_name),
                ],
                PathBuf::from(&snapshot.source_repository_path),
                &disabled_hooks,
            ),
        )?;
        if !snapshot.ownership_claimed && (snapshot.worktree_path.exists() || branch.result.success)
        {
            return Err(WorkerError::new(
                "worktree_ownership_conflict",
                "The expected branch or worktree appeared before Quorum persisted its ownership claim. Quorum left both unchanged.",
            ));
        }
        let claim = self.ensure_ownership_claim(snapshot)?;
        if !snapshot.worktree_path.exists() {
            reject_executable_checkout_filters(&git, &snapshot.source_repository_path)?;
            let arguments = if branch.result.success {
                validate_owned_branch(
                    &snapshot.source_repository_path,
                    &snapshot.branch_name,
                    &snapshot.base_commit,
                )?;
                vec![
                    "-C".to_owned(),
                    snapshot.source_repository_path.clone(),
                    "worktree".to_owned(),
                    "add".to_owned(),
                    snapshot.worktree_path.to_string_lossy().into_owned(),
                    snapshot.branch_name.clone(),
                ]
            } else {
                vec![
                    "-C".to_owned(),
                    snapshot.source_repository_path.clone(),
                    "worktree".to_owned(),
                    "add".to_owned(),
                    "-b".to_owned(),
                    snapshot.branch_name.clone(),
                    snapshot.worktree_path.to_string_lossy().into_owned(),
                    snapshot.base_commit.clone(),
                ]
            };
            let added = self.execute_command(
                snapshot,
                control,
                "preparing",
                trusted_provisioning_git_request(
                    git,
                    arguments,
                    PathBuf::from(&snapshot.source_repository_path),
                    &disabled_hooks,
                ),
            )?;
            if !added.result.success {
                return Err(process_failure(
                    "worktree_creation_failed",
                    "Git could not create the managed worktree. Quorum left all existing branches and worktrees unchanged.",
                    &added.result,
                ));
            }
        }
        self.verify_ownership_claim(&claim)?;
        let identity = validate_owned_worktree(
            &snapshot.source_repository_path,
            &snapshot.branch_name,
            &snapshot.worktree_path,
            snapshot.git_metadata.as_ref(),
        )?;
        self.mark_ownership_verified(&snapshot.run_id, &identity)
            .map_err(WorkerError::database)
    }

    fn disabled_git_hooks_directory(&self, run_id: &str) -> Result<PathBuf, WorkerError> {
        let app_data = fs::canonicalize(self.store.app_data_dir()).map_err(|error| {
            WorkerError::new(
                "worktree_provisioning_failed",
                format!("Could not resolve Quorum application data: {error}"),
            )
        })?;
        let provisioning = app_data.join("worktree-provisioning");
        secure_directory(&provisioning, &app_data)?;
        let run_directory = provisioning.join(run_id);
        secure_directory(&run_directory, &provisioning)?;
        let hooks = run_directory.join("disabled-hooks");
        secure_directory(&hooks, &run_directory)?;
        let mut entries = fs::read_dir(&hooks).map_err(|error| {
            WorkerError::new(
                "worktree_provisioning_failed",
                format!(
                    "Could not inspect the disabled Git hooks directory {}: {error}",
                    hooks.display()
                ),
            )
        })?;
        if entries
            .next()
            .transpose()
            .map_err(|error| {
                WorkerError::new(
                    "worktree_provisioning_failed",
                    format!(
                        "Could not inspect the disabled Git hooks directory {}: {error}",
                        hooks.display()
                    ),
                )
            })?
            .is_some()
        {
            return Err(WorkerError::new(
                "unsafe_git_configuration",
                "Quorum's controlled disabled-hooks directory is not empty. Remove the unexpected contents and retry.",
            ));
        }
        Ok(hooks)
    }

    fn ensure_ownership_claim(
        &self,
        snapshot: &WorkerSnapshot,
    ) -> Result<OwnershipClaim, WorkerError> {
        let timestamp = now();
        self.store
            .with_connection(|connection| {
                connection.execute(
                    "UPDATE execution_runs
                     SET ownership_claimed_at = COALESCE(ownership_claimed_at, ?2),
                         updated_at = ?2
                     WHERE run_id = ?1",
                    params![snapshot.run_id, timestamp],
                )?;
                Ok(())
            })
            .map_err(WorkerError::database)?;
        let claim = OwnershipClaim {
            run_id: snapshot.run_id.clone(),
            token: snapshot.ownership_token.clone(),
            source_repository_path: snapshot.source_repository_path.clone(),
            base_commit: snapshot.base_commit.clone(),
            branch_name: snapshot.branch_name.clone(),
            worktree_path: snapshot.worktree_path.to_string_lossy().into_owned(),
        };
        let path = self.ownership_claim_path(&snapshot.run_id)?;
        self.persist_or_verify_ownership_claim(&path, &claim)?;
        Ok(claim)
    }

    fn ensure_resume_ownership_claim(
        &self,
        claim: &OwnershipClaim,
        worktree_path: &Path,
    ) -> Result<(), WorkerError> {
        self.validate_managed_worktree_location(worktree_path)?;
        let path = self.ownership_claim_path(&claim.run_id)?;
        self.persist_or_verify_ownership_claim(&path, claim)
    }

    fn persist_or_verify_ownership_claim(
        &self,
        path: &Path,
        claim: &OwnershipClaim,
    ) -> Result<(), WorkerError> {
        match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(mut file) => {
                let bytes = serde_json::to_vec(&claim).map_err(|error| {
                    WorkerError::new(
                        "worktree_ownership",
                        format!("Could not serialize the worktree ownership claim: {error}"),
                    )
                })?;
                file.write_all(&bytes).map_err(|error| {
                    WorkerError::new(
                        "worktree_ownership",
                        format!("Could not persist {}: {error}", path.display()),
                    )
                })?;
                file.sync_all().map_err(|error| {
                    WorkerError::new(
                        "worktree_ownership",
                        format!("Could not durably persist {}: {error}", path.display()),
                    )
                })?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                self.verify_ownership_claim(claim)?;
            }
            Err(error) => {
                return Err(WorkerError::new(
                    "worktree_ownership",
                    format!("Could not create {}: {error}", path.display()),
                ));
            }
        }
        Ok(())
    }

    fn validate_managed_worktree_location(&self, worktree_path: &Path) -> Result<(), WorkerError> {
        let app_data = self.store.app_data_dir();
        let app_data_root = fs::canonicalize(app_data).map_err(|error| {
            WorkerError::new(
                "worktree_ownership_conflict",
                format!("Could not resolve Quorum application data: {error}"),
            )
        })?;
        let worktrees = app_data.join("worktrees");
        let metadata = fs::symlink_metadata(&worktrees).map_err(|error| {
            WorkerError::new(
                "worktree_ownership_conflict",
                format!(
                    "Could not inspect Quorum's managed worktree directory {}: {error}",
                    worktrees.display()
                ),
            )
        })?;
        if !metadata.file_type().is_dir() || worktree_path.parent() != Some(worktrees.as_path()) {
            return Err(WorkerError::new(
                "worktree_ownership_conflict",
                "The persisted managed worktree path is not a direct child of Quorum's real worktree directory.",
            ));
        }
        let worktrees_root = fs::canonicalize(&worktrees).map_err(|error| {
            WorkerError::new(
                "worktree_ownership_conflict",
                format!("Could not resolve {}: {error}", worktrees.display()),
            )
        })?;
        if !worktrees_root.starts_with(&app_data_root) {
            return Err(WorkerError::new(
                "worktree_ownership_conflict",
                "Quorum's managed worktree directory escapes application data.",
            ));
        }
        match fs::symlink_metadata(worktree_path) {
            Ok(metadata) if metadata.file_type().is_dir() => {
                let resolved = fs::canonicalize(worktree_path).map_err(|error| {
                    WorkerError::new(
                        "worktree_ownership_conflict",
                        format!(
                            "Could not resolve the persisted managed worktree {}: {error}",
                            worktree_path.display()
                        ),
                    )
                })?;
                if resolved.parent() != Some(worktrees_root.as_path()) {
                    return Err(WorkerError::new(
                        "worktree_ownership_conflict",
                        "The persisted managed worktree resolves outside Quorum's worktree directory.",
                    ));
                }
            }
            Ok(_) => {
                return Err(WorkerError::new(
                    "worktree_ownership_conflict",
                    "The persisted managed worktree path is a symlink or non-directory entry.",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(WorkerError::new(
                    "worktree_ownership_conflict",
                    format!(
                        "Could not inspect the persisted managed worktree {}: {error}",
                        worktree_path.display()
                    ),
                ));
            }
        }
        Ok(())
    }

    fn ownership_claim_path(&self, run_id: &str) -> Result<PathBuf, WorkerError> {
        let directory = self.store.app_data_dir().join("worktree-ownership");
        let root = fs::canonicalize(self.store.app_data_dir()).map_err(|error| {
            WorkerError::new(
                "worktree_ownership",
                format!("Could not resolve Quorum application data: {error}"),
            )
        })?;
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => {
                return Err(WorkerError::new(
                    "worktree_ownership",
                    format!("{} is not a real directory.", directory.display()),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&directory).map_err(|error| {
                    WorkerError::new(
                        "worktree_ownership",
                        format!("Could not create {}: {error}", directory.display()),
                    )
                })?;
            }
            Err(error) => {
                return Err(WorkerError::new(
                    "worktree_ownership",
                    format!("Could not inspect {}: {error}", directory.display()),
                ));
            }
        }
        if !fs::canonicalize(&directory)
            .map_err(|error| {
                WorkerError::new(
                    "worktree_ownership",
                    format!("Could not resolve {}: {error}", directory.display()),
                )
            })?
            .starts_with(root)
        {
            return Err(WorkerError::new(
                "worktree_ownership",
                "The worktree ownership directory escapes Quorum application data.",
            ));
        }
        Ok(directory.join(format!("{run_id}.json")))
    }

    fn verify_ownership_claim(&self, expected: &OwnershipClaim) -> Result<(), WorkerError> {
        let path = self.ownership_claim_path(&expected.run_id)?;
        let root = fs::canonicalize(self.store.app_data_dir()).map_err(|error| {
            WorkerError::new(
                "worktree_ownership",
                format!("Could not resolve Quorum application data: {error}"),
            )
        })?;
        let file = open_regular_contained(&path, &root, "worktree ownership claim")?;
        let mut bytes = Vec::new();
        file.take(16 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                WorkerError::new(
                    "worktree_ownership",
                    format!("Could not read {}: {error}", path.display()),
                )
            })?;
        if bytes.len() > 16 * 1024 {
            return Err(WorkerError::new(
                "worktree_ownership_conflict",
                "The persisted worktree ownership claim is unexpectedly large.",
            ));
        }
        let actual: OwnershipClaim = serde_json::from_slice(&bytes).map_err(|error| {
            WorkerError::new(
                "worktree_ownership_conflict",
                format!("The persisted worktree ownership claim is invalid: {error}"),
            )
        })?;
        if &actual != expected {
            return Err(WorkerError::new(
                "worktree_ownership_conflict",
                "The persisted branch/worktree ownership claim does not match this execution run.",
            ));
        }
        Ok(())
    }

    fn mark_ownership_verified(
        &self,
        run_id: &str,
        identity: &GitMetadataIdentity,
    ) -> Result<(), AppError> {
        let identity = serde_json::to_string(identity).map_err(|error| {
            AppError::database(format!(
                "Could not persist the managed worktree Git identity: {error}"
            ))
        })?;
        self.store.with_connection(|connection| {
            connection.execute(
                "UPDATE execution_runs
                 SET ownership_verified_at = ?2, git_metadata_json = ?3, updated_at = ?2
                 WHERE run_id = ?1",
                params![run_id, now(), identity],
            )?;
            Ok(())
        })
    }

    #[allow(clippy::too_many_lines)]
    fn run_builder(
        &self,
        snapshot: &WorkerSnapshot,
        control: &RunControl,
    ) -> Result<(), WorkerError> {
        let remediation = snapshot.current_step == "remediating";
        let prompt = snapshot
            .pending_builder_prompt
            .clone()
            .unwrap_or_else(|| initial_builder_prompt(&snapshot.plan_markdown));
        let timestamp = now();
        self.store
            .with_connection(|connection| {
                let transaction = connection.unchecked_transaction()?;
                transaction.execute(
                    "UPDATE execution_runs
                     SET status = ?2, current_step = ?3,
                         builder_session_state = CASE
                           WHEN builder_session_state = 'resumable' THEN 'resumable'
                           ELSE 'launching'
                         END,
                         pending_builder_prompt = ?5, updated_at = ?4
                     WHERE run_id = ?1",
                    params![
                        snapshot.run_id,
                        if remediation {
                            "remediating"
                        } else {
                            "building"
                        },
                        if remediation {
                            "remediating"
                        } else {
                            "building"
                        },
                        timestamp,
                        prompt
                    ],
                )?;
                transaction.execute(
                    "UPDATE runs SET phase = 'building', outcome = 'running', updated_at = ?2
                     WHERE id = ?1",
                    params![snapshot.run_id, timestamp],
                )?;
                append_event(
                    &transaction,
                    &snapshot.run_id,
                    if remediation {
                        "builder_remediation_started"
                    } else {
                        "builder_started"
                    },
                    &json!({
                        "sessionName": snapshot.builder_session_name,
                        "model": snapshot.builder_model,
                        "iteration": snapshot.iteration,
                        "resumed": snapshot.builder_session_state == "resumable",
                    }),
                    &timestamp,
                )?;
                transaction.commit()?;
                Ok(())
            })
            .map_err(WorkerError::database)?;

        let arguments = builder_arguments(snapshot, &prompt);
        let request = copilot_process_request(snapshot, arguments, "builder")?;
        let execution = match self.execute_command(
            snapshot,
            control,
            if remediation {
                "remediating"
            } else {
                "building"
            },
            request,
        ) {
            Ok(execution) => execution,
            Err(error) => {
                if snapshot.builder_session_state != "resumable" {
                    self.reset_session_launch(&snapshot.run_id, "builder")
                        .map_err(WorkerError::database)?;
                }
                return Err(error);
            }
        };
        if !execution.result.success {
            if snapshot.builder_session_state != "resumable" {
                self.reset_session_launch(&snapshot.run_id, "builder")
                    .map_err(WorkerError::database)?;
            }
            return Err(process_failure(
                "builder_failed",
                "The Copilot builder did not complete successfully.",
                &execution.result,
            ));
        }
        if !copilot_session_confirmed(&execution.result) {
            if snapshot.builder_session_state != "resumable" {
                self.reset_session_launch(&snapshot.run_id, "builder")
                    .map_err(WorkerError::database)?;
            }
            return Err(WorkerError::new(
                "copilot_session_unconfirmed",
                "The Copilot builder exited without complete successful session evidence. Quorum will use a fresh session on retry.",
            ));
        }
        self.confirm_session_launch(&snapshot.run_id, "builder")
            .map_err(WorkerError::database)?;
        if remediation {
            if let Some(before) = snapshot.remediation_diff_hash.as_deref() {
                let after = Self::base_evidence(snapshot, control)?;
                if after.digest == before {
                    return Err(WorkerError::new(
                        "no_material_fix",
                        "The builder completed remediation without changing the base diff. Quorum stopped the loop; resolve findings explicitly or resume after making an intentional change.",
                    ));
                }
            }
        }
        let timestamp = now();
        self.store
            .with_connection(|connection| {
                let transaction = connection.unchecked_transaction()?;
                transaction.execute(
                    "UPDATE execution_runs
                     SET status = 'verifying', current_step = 'verifying',
                         builder_completed_at = ?2, pending_builder_prompt = NULL,
                         remediation_diff_hash = NULL, error_code = NULL,
                         error_message = NULL, verified_state_digest = NULL,
                         reviewed_state_digest = NULL, updated_at = ?2
                     WHERE run_id = ?1",
                    params![snapshot.run_id, timestamp],
                )?;
                append_event(
                    &transaction,
                    &snapshot.run_id,
                    "builder_completed",
                    &json!({
                        "sessionName": snapshot.builder_session_name,
                        "iteration": snapshot.iteration,
                    }),
                    &timestamp,
                )?;
                transaction.commit()?;
                Ok(())
            })
            .map_err(WorkerError::database)
    }

    fn run_verification(
        &self,
        snapshot: &WorkerSnapshot,
        control: &RunControl,
    ) -> Result<(), WorkerError> {
        self.transition(
            &snapshot.run_id,
            "building",
            "verifying",
            "verifying",
            "verification_started",
            &json!({
                "program": snapshot.verification_program,
                "arguments": snapshot.verification_arguments,
                "iteration": snapshot.iteration,
            }),
        )
        .map_err(WorkerError::database)?;
        let before_verification = Self::base_evidence(snapshot, control)?;
        let execution = self.execute_command(
            snapshot,
            control,
            "verifying",
            verification_process_request(snapshot)?,
        )?;
        let verification_state = Self::verified_state_after_command(
            snapshot,
            control,
            &before_verification.digest,
            execution.result.success,
        );
        let verified_state_digest = verification_state
            .as_ref()
            .ok()
            .and_then(|digest| digest.as_deref());
        let event_kind = if !execution.result.success {
            "verification_failed"
        } else if verification_state.is_err() {
            "verification_state_rejected"
        } else {
            "verification_succeeded"
        };
        let timestamp = now();
        self.store
            .with_connection(|connection| {
                let transaction = connection.unchecked_transaction()?;
                transaction.execute(
                    "UPDATE execution_runs
                     SET latest_verification_command_id = ?2,
                         verified_state_digest = ?3, reviewed_state_digest = NULL,
                         updated_at = ?4
                     WHERE run_id = ?1",
                    params![
                        snapshot.run_id,
                        execution.id,
                        verified_state_digest,
                        timestamp
                    ],
                )?;
                append_event(
                    &transaction,
                    &snapshot.run_id,
                    event_kind,
                    &json!({
                        "commandId": execution.id,
                        "exitCode": execution.result.exit_code,
                        "status": execution.result.status,
                        "iteration": snapshot.iteration,
                    }),
                    &timestamp,
                )?;
                transaction.commit()?;
                Ok(())
            })
            .map_err(WorkerError::database)?;
        verification_state?;
        if execution.result.success {
            self.transition(
                &snapshot.run_id,
                "reviewing",
                "reviewing",
                "reviewing",
                "review_requested",
                &json!({"iteration": snapshot.iteration}),
            )
            .map_err(WorkerError::database)
        } else if snapshot.iteration >= snapshot.max_iterations {
            Err(process_failure(
                "verification_failed",
                "Verification failed after the bounded remediation limit.",
                &execution.result,
            ))
        } else {
            let evidence = process_evidence(&execution.result);
            let prompt = verification_remediation_prompt(
                &snapshot.verification_program,
                &snapshot.verification_arguments,
                &evidence,
            );
            let baseline = Self::base_evidence(snapshot, control)?;
            self.schedule_remediation(snapshot, &prompt, &baseline.digest, "verification")
                .map_err(WorkerError::database)
        }
    }

    fn verified_state_after_command(
        snapshot: &WorkerSnapshot,
        control: &RunControl,
        before_verification: &str,
        succeeded: bool,
    ) -> Result<Option<String>, WorkerError> {
        if !succeeded {
            return Ok(None);
        }
        let after = Self::base_evidence(snapshot, control)?;
        if after.digest != before_verification {
            return Err(WorkerError::new(
                "state_changed_during_verification",
                "The managed worktree changed while verification was running. Quorum discarded the successful result and blocked delivery until verification reruns against unchanged content.",
            ));
        }
        Ok(Some(after.digest))
    }

    #[allow(clippy::too_many_lines)]
    fn run_review(
        &self,
        snapshot: &WorkerSnapshot,
        control: &RunControl,
    ) -> Result<(), WorkerError> {
        let evidence = Self::base_evidence(snapshot, control)?;
        let verified_state_digest = snapshot.verified_state_digest.as_deref().ok_or_else(|| {
            WorkerError::new(
                "verification_state_missing",
                "Delivery review has no persisted full-state digest from successful verification.",
            )
        })?;
        if evidence.digest != verified_state_digest {
            self.rewind_delivery_state(&snapshot.run_id)
                .map_err(WorkerError::database)?;
            return Err(WorkerError::new(
                "state_changed_after_verification",
                "The managed worktree changed after successful verification. Quorum invalidated prior verification and review state; resume to verify and review the current content.",
            ));
        }
        let verification = self
            .verification_evidence(&snapshot.run_id)
            .map_err(WorkerError::database)?;
        let prompt = reviewer_prompt(
            &snapshot.plan_markdown,
            &snapshot.acceptance_intent,
            &snapshot.base_commit,
            &evidence.review_diff,
            &verification,
            snapshot.iteration,
        );
        let timestamp = now();
        self.store
            .with_connection(|connection| {
                let transaction = connection.unchecked_transaction()?;
                transaction.execute(
                    "UPDATE execution_runs
                     SET status = 'reviewing', current_step = 'reviewing',
                         reviewer_session_state = CASE
                           WHEN reviewer_session_state = 'resumable' THEN 'resumable'
                           ELSE 'launching'
                         END,
                         updated_at = ?2
                     WHERE run_id = ?1",
                    params![snapshot.run_id, timestamp],
                )?;
                append_event(
                    &transaction,
                    &snapshot.run_id,
                    "reviewer_started",
                    &json!({
                        "sessionName": snapshot.reviewer_session_name,
                        "model": snapshot.reviewer_model,
                        "iteration": snapshot.iteration,
                        "focused": snapshot.iteration > 0,
                        "resumed": snapshot.reviewer_session_state == "resumable",
                    }),
                    &timestamp,
                )?;
                transaction.commit()?;
                Ok(())
            })
            .map_err(WorkerError::database)?;
        let request =
            copilot_process_request(snapshot, reviewer_arguments(snapshot, &prompt), "reviewer")?;
        let execution = match self.execute_command(snapshot, control, "reviewing", request) {
            Ok(execution) => execution,
            Err(error) => {
                if snapshot.reviewer_session_state != "resumable" {
                    self.reset_session_launch(&snapshot.run_id, "reviewer")
                        .map_err(WorkerError::database)?;
                }
                return Err(error);
            }
        };
        if !execution.result.success {
            if snapshot.reviewer_session_state != "resumable" {
                self.reset_session_launch(&snapshot.run_id, "reviewer")
                    .map_err(WorkerError::database)?;
            }
            return Err(process_failure(
                "reviewer_failed",
                "The independent adversarial reviewer did not complete successfully.",
                &execution.result,
            ));
        }
        if execution.result.capture_truncated {
            if snapshot.reviewer_session_state != "resumable" {
                self.reset_session_launch(&snapshot.run_id, "reviewer")
                    .map_err(WorkerError::database)?;
            }
            return Err(WorkerError::new(
                "review_output_incomplete",
                "The reviewer output exceeded Quorum's complete-capture bound. Delivery is blocked because the structured review result may be incomplete.",
            ));
        }
        if !copilot_session_confirmed(&execution.result) {
            if snapshot.reviewer_session_state != "resumable" {
                self.reset_session_launch(&snapshot.run_id, "reviewer")
                    .map_err(WorkerError::database)?;
            }
            return Err(WorkerError::new(
                "copilot_session_unconfirmed",
                "The Copilot reviewer exited without complete successful session evidence. Quorum will use a fresh session on retry.",
            ));
        }
        self.confirm_session_launch(&snapshot.run_id, "reviewer")
            .map_err(WorkerError::database)?;
        let review = parse_review_jsonl(&String::from_utf8_lossy(&execution.result.stdout))?;
        let blocking = self
            .persist_review(snapshot, &execution.id, &review, &evidence.digest)
            .map_err(WorkerError::database)?;
        if blocking.is_empty() {
            let current = Self::base_evidence(snapshot, control)?;
            if current.digest != evidence.digest || current.digest != verified_state_digest {
                self.rewind_delivery_state(&snapshot.run_id)
                    .map_err(WorkerError::database)?;
                return Err(WorkerError::new(
                    "state_changed_after_review",
                    "The managed worktree changed after verification or adversarial review. Quorum invalidated both digests and will not mark delivery ready.",
                ));
            }
            let timestamp = now();
            self.store
                .with_connection(|connection| {
                    let transaction = connection.unchecked_transaction()?;
                    mark_ready(&transaction, &snapshot.run_id, &current.digest, &timestamp)?;
                    transaction.commit()?;
                    Ok(())
                })
                .map_err(WorkerError::database)
        } else if snapshot.iteration >= snapshot.max_iterations {
            Err(WorkerError::new(
                "blocking_findings",
                format!(
                    "{} blocking review finding(s) remain after the bounded remediation limit. Resolve each finding with a disposition note before delivery.",
                    blocking.len()
                ),
            ))
        } else {
            let prompt = findings_remediation_prompt(&review.summary, &blocking)?;
            self.schedule_remediation(snapshot, &prompt, &evidence.digest, "blocking_findings")
                .map_err(WorkerError::database)
        }
    }

    fn base_evidence(
        snapshot: &WorkerSnapshot,
        control: &RunControl,
    ) -> Result<BaseEvidence, WorkerError> {
        Self::base_evidence_with_interpass(snapshot, control, || {})
    }

    fn base_evidence_with_interpass(
        snapshot: &WorkerSnapshot,
        control: &RunControl,
        interpass: impl FnOnce(),
    ) -> Result<BaseEvidence, WorkerError> {
        let git = resolve_executable("git").ok_or_else(|| {
            WorkerError::new(
                "missing_git",
                "Git executable `git` is no longer available on PATH.",
            )
        })?;
        validate_snapshot_git_metadata(snapshot)?;
        let identity = snapshot.git_metadata.as_ref().ok_or_else(|| {
            WorkerError::new(
                "git_metadata_changed",
                "The managed worktree has no persisted trusted Git metadata identity.",
            )
        })?;
        let evidence_directory = secure_runtime_directory(&snapshot.worktree_path, "evidence")?;
        let _cleanup = EvidenceArtifactCleanup::new(&evidence_directory)?;
        let mut budget = EvidenceBudget::new();
        let before =
            Self::capture_base_evidence_once(&git, snapshot, identity, &mut budget, control)?;
        interpass();
        let after =
            Self::capture_base_evidence_once(&git, snapshot, identity, &mut budget, control)?;
        if before.digest != after.digest || before.review_diff != after.review_diff {
            return Err(evidence_state_changed());
        }
        validate_snapshot_git_metadata(snapshot)?;
        Ok(after)
    }

    fn capture_base_evidence_once(
        git: &str,
        snapshot: &WorkerSnapshot,
        identity: &GitMetadataIdentity,
        budget: &mut EvidenceBudget,
        control: &RunControl,
    ) -> Result<BaseEvidence, WorkerError> {
        let tracked_arguments = [
            "diff",
            "--no-ext-diff",
            "--binary",
            &snapshot.base_commit,
            "--",
        ];
        let untracked_exclusion = format!("--exclude={RUNTIME_DIRECTORY}/**");
        let untracked_arguments = [
            "ls-files",
            "-z",
            "--others",
            "--exclude-standard",
            untracked_exclusion.as_str(),
        ];
        let tracked = run_trusted_git_capture(
            git,
            &snapshot.worktree_path,
            identity,
            &tracked_arguments,
            budget,
            Some(control),
            Some(MAX_REVIEW_DIFF_BYTES),
        )?;
        let untracked = run_trusted_git_capture(
            git,
            &snapshot.worktree_path,
            identity,
            &untracked_arguments,
            budget,
            Some(control),
            None,
        )?;
        let evidence = collect_base_evidence_bytes(
            &snapshot.worktree_path,
            &tracked,
            &untracked,
            budget,
            Some(control),
        )?;
        let tracked_after = run_trusted_git_capture(
            git,
            &snapshot.worktree_path,
            identity,
            &tracked_arguments,
            budget,
            Some(control),
            Some(MAX_REVIEW_DIFF_BYTES),
        )?;
        let untracked_after = run_trusted_git_capture(
            git,
            &snapshot.worktree_path,
            identity,
            &untracked_arguments,
            budget,
            Some(control),
            None,
        )?;
        if tracked != tracked_after || untracked != untracked_after {
            return Err(evidence_state_changed());
        }
        Ok(evidence)
    }

    fn verification_evidence(&self, run_id: &str) -> Result<String, AppError> {
        self.store.with_connection(|connection| {
            let command = connection
                .query_row(
                    "SELECT execution_commands.id, execution_commands.program,
                            execution_commands.args_json, execution_commands.status,
                            execution_commands.exit_code,
                            execution_commands.output_truncated
                     FROM execution_runs
                     JOIN execution_commands
                       ON execution_commands.id = execution_runs.latest_verification_command_id
                     WHERE execution_runs.run_id = ?1",
                    [run_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, Option<i32>>(4)?,
                            row.get::<_, bool>(5)?,
                        ))
                    },
                )
                .optional()?
                .ok_or_else(|| {
                    StoreError::App(AppError::database(
                        "Verification evidence is missing for adversarial review.",
                    ))
                })?;
            if command.5 {
                return Err(StoreError::App(AppError::database(
                    "Verification output exceeded Quorum's complete evidence bound. Delivery is blocked because adversarial review cannot receive complete verification evidence.",
                )));
            }
            let mut statement = connection.prepare(
                "SELECT stream, text FROM execution_logs
                 WHERE command_id = ?1 ORDER BY sequence",
            )?;
            let output = statement
                .query_map([&command.0], |row| {
                    Ok(format!(
                        "[{}] {}",
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
                .join("");
            Ok(format!(
                "program: {}\narguments: {}\nstatus: {}\nexit_code: {:?}\noutput:\n{}",
                command.1, command.2, command.3, command.4, output
            ))
        })
    }

    fn confirm_session_launch(&self, run_id: &str, role: &str) -> Result<(), AppError> {
        let timestamp = now();
        let sql = match role {
            "builder" => {
                "UPDATE execution_runs
                 SET builder_session_state = 'resumable',
                     builder_session_started_at = COALESCE(builder_session_started_at, ?2),
                     updated_at = ?2
                 WHERE run_id = ?1"
            }
            "reviewer" => {
                "UPDATE execution_runs
                 SET reviewer_session_state = 'resumable',
                     reviewer_session_started_at = COALESCE(reviewer_session_started_at, ?2),
                     updated_at = ?2
                 WHERE run_id = ?1"
            }
            _ => return Err(AppError::database("Unknown Copilot execution role.")),
        };
        self.store.with_connection(|connection| {
            connection.execute(sql, params![run_id, timestamp])?;
            Ok(())
        })
    }

    fn reset_session_launch(&self, run_id: &str, role: &str) -> Result<(), AppError> {
        let sql = match role {
            "builder" => {
                "UPDATE execution_runs
                 SET builder_session_state = 'not_started', updated_at = ?2
                 WHERE run_id = ?1 AND builder_session_state = 'launching'"
            }
            "reviewer" => {
                "UPDATE execution_runs
                 SET reviewer_session_state = 'not_started', updated_at = ?2
                 WHERE run_id = ?1 AND reviewer_session_state = 'launching'"
            }
            _ => return Err(AppError::database("Unknown Copilot execution role.")),
        };
        self.store.with_connection(|connection| {
            connection.execute(sql, params![run_id, now()])?;
            Ok(())
        })
    }

    fn schedule_remediation(
        &self,
        snapshot: &WorkerSnapshot,
        prompt: &str,
        baseline_hash: &str,
        reason: &str,
    ) -> Result<(), AppError> {
        let timestamp = now();
        self.store.with_connection(|connection| {
            let transaction = connection.unchecked_transaction()?;
            transaction.execute(
                "UPDATE execution_runs
                 SET status = 'remediating', current_step = 'remediating',
                     iteration = iteration + 1, pending_builder_prompt = ?2,
                     remediation_diff_hash = ?3, error_code = NULL,
                     error_message = NULL, verified_state_digest = NULL,
                     reviewed_state_digest = NULL, updated_at = ?4
                 WHERE run_id = ?1",
                params![snapshot.run_id, prompt, baseline_hash, timestamp],
            )?;
            transaction.execute(
                "UPDATE runs SET phase = 'building', outcome = 'running', updated_at = ?2
                 WHERE id = ?1",
                params![snapshot.run_id, timestamp],
            )?;
            append_event(
                &transaction,
                &snapshot.run_id,
                "remediation_requested",
                &json!({
                    "reason": reason,
                    "nextIteration": snapshot.iteration + 1,
                    "maxIterations": snapshot.max_iterations,
                    "builderSessionName": snapshot.builder_session_name,
                }),
                &timestamp,
            )?;
            transaction.commit()?;
            Ok(())
        })
    }

    fn rewind_delivery_state(&self, run_id: &str) -> Result<(), AppError> {
        self.store.with_connection(|connection| {
            connection.execute(
                "UPDATE execution_runs
                 SET current_step = 'verifying', verified_state_digest = NULL,
                     reviewed_state_digest = NULL, latest_verification_command_id = NULL,
                     updated_at = ?2
                 WHERE run_id = ?1",
                params![run_id, now()],
            )?;
            Ok(())
        })
    }

    #[allow(clippy::too_many_lines)]
    fn persist_review(
        &self,
        snapshot: &WorkerSnapshot,
        command_id: &str,
        review: &ReviewEnvelope,
        reviewed_state_digest: &str,
    ) -> Result<Vec<ReviewFinding>, AppError> {
        let timestamp = now();
        self.store.with_connection(|connection| {
            let transaction = connection.unchecked_transaction()?;
            transaction.execute(
                "INSERT INTO execution_reviews (
                   id, run_id, iteration, command_id, summary, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(run_id, iteration) DO UPDATE SET
                   command_id = excluded.command_id,
                   summary = excluded.summary,
                   created_at = excluded.created_at",
                params![
                    Uuid::new_v4().to_string(),
                    snapshot.run_id,
                    snapshot.iteration,
                    command_id,
                    review.summary,
                    timestamp
                ],
            )?;
            let returned_ids: HashSet<&str> = review
                .findings
                .iter()
                .map(|finding| finding.id.as_str())
                .collect();
            let mut prior = transaction.prepare(
                "SELECT external_id FROM execution_findings
                 WHERE run_id = ?1 AND status = 'open'",
            )?;
            let prior_ids = prior
                .query_map([&snapshot.run_id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            drop(prior);
            for prior_id in prior_ids {
                if !returned_ids.contains(prior_id.as_str()) {
                    transaction.execute(
                        "UPDATE execution_findings
                         SET status = 'fixed', updated_at = ?3
                         WHERE run_id = ?1 AND external_id = ?2 AND status = 'open'",
                        params![snapshot.run_id, prior_id, timestamp],
                    )?;
                }
            }
            for finding in &review.findings {
                transaction.execute(
                    "INSERT INTO execution_findings (
                       id, run_id, external_id, severity, title, body, path, line,
                       status, first_seen_iteration, last_seen_iteration,
                       created_at, updated_at
                     ) VALUES (
                       ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'open', ?9, ?9, ?10, ?10
                     )
                     ON CONFLICT(run_id, external_id) DO UPDATE SET
                       severity = excluded.severity,
                       title = excluded.title,
                       body = excluded.body,
                       path = excluded.path,
                       line = excluded.line,
                       status = CASE
                         WHEN execution_findings.status = 'resolved' THEN 'resolved'
                         ELSE 'open'
                       END,
                       last_seen_iteration = excluded.last_seen_iteration,
                       updated_at = excluded.updated_at",
                    params![
                        Uuid::new_v4().to_string(),
                        snapshot.run_id,
                        finding.id,
                        finding.severity,
                        finding.title,
                        finding.body,
                        finding.path,
                        finding.line,
                        snapshot.iteration,
                        timestamp
                    ],
                )?;
            }
            let digest_persisted = transaction.execute(
                "UPDATE execution_runs
                 SET reviewed_state_digest = ?2, updated_at = ?3
                 WHERE run_id = ?1 AND verified_state_digest = ?2",
                params![snapshot.run_id, reviewed_state_digest, timestamp],
            )?;
            if digest_persisted != 1 {
                return Err(StoreError::App(AppError::conflict(
                    "The reviewed state no longer matches the persisted verified state.",
                )));
            }
            append_event(
                &transaction,
                &snapshot.run_id,
                "review_completed",
                &json!({
                    "iteration": snapshot.iteration,
                    "summary": review.summary,
                    "findingCount": review.findings.len(),
                    "blockingCount": review.findings.iter()
                        .filter(|finding| finding.severity == "blocking")
                        .count(),
                }),
                &timestamp,
            )?;
            transaction.commit()?;
            Ok(review
                .findings
                .iter()
                .filter(|finding| finding.severity == "blocking")
                .cloned()
                .collect())
        })
    }

    fn transition(
        &self,
        run_id: &str,
        phase: &str,
        status: &str,
        step: &str,
        event_kind: &str,
        payload: &Value,
    ) -> Result<(), AppError> {
        let timestamp = now();
        self.store.with_connection(|connection| {
            let transaction = connection.unchecked_transaction()?;
            transaction.execute(
                "UPDATE execution_runs
                 SET status = ?2, current_step = ?3, error_code = NULL,
                     error_message = NULL, updated_at = ?4
                 WHERE run_id = ?1",
                params![run_id, status, step, timestamp],
            )?;
            transaction.execute(
                "UPDATE runs SET phase = ?2, outcome = 'running', updated_at = ?3
                 WHERE id = ?1",
                params![run_id, phase, timestamp],
            )?;
            append_event(&transaction, run_id, event_kind, payload, &timestamp)?;
            transaction.commit()?;
            Ok(())
        })
    }

    #[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
    fn execute_command(
        &self,
        snapshot: &WorkerSnapshot,
        control: &RunControl,
        phase: &str,
        mut request: ProcessRequest,
    ) -> Result<CommandExecution, WorkerError> {
        if request.untrusted {
            validate_snapshot_git_metadata(snapshot)?;
        }
        request.lease_path = Some(
            self.store
                .run_lease_path(&snapshot.run_id)
                .map_err(WorkerError::database)?,
        );
        let command_id = Uuid::new_v4().to_string();
        let timestamp = now();
        let attempt_id = self
            .store
            .with_connection(|connection| {
                connection
                    .query_row(
                        "SELECT id FROM execution_attempts
                         WHERE run_id = ?1 AND status = 'running'
                         ORDER BY number DESC LIMIT 1",
                        [&snapshot.run_id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                    .ok_or_else(|| {
                        StoreError::App(AppError::database(
                            "The execution worker has no current owned attempt.",
                        ))
                    })
            })
            .map_err(WorkerError::database)?;
        let arguments_json = serde_json::to_string(&request.arguments).map_err(|error| {
            WorkerError::new(
                "database",
                format!("Could not serialize command arguments: {error}"),
            )
        })?;
        self.store
            .with_connection(|connection| {
                let ordinal: usize = connection.query_row(
                    "SELECT COALESCE(MAX(ordinal), -1) + 1
                     FROM execution_commands WHERE execution_attempt_id = ?1",
                    [&attempt_id],
                    |row| row.get(0),
                )?;
                connection.execute(
                    "INSERT INTO execution_commands (
                       id, run_id, execution_attempt_id, ordinal, phase, program,
                       args_json, cwd, status, started_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'running', ?9)",
                    params![
                        command_id,
                        snapshot.run_id,
                        attempt_id,
                        ordinal,
                        phase,
                        request.program,
                        arguments_json,
                        request.cwd.to_string_lossy(),
                        timestamp
                    ],
                )?;
                Ok(())
            })
            .map_err(WorkerError::database)?;

        let mut sequence = 0_usize;
        let mut persisted_bytes = 0_usize;
        let mut persist_error = None;
        let mut truncation_logged = false;
        let copilot_json_stream = request
            .arguments
            .iter()
            .any(|argument| argument == "--stream=on");
        let result = {
            let mut persist_chunk = |chunk: ProcessChunk| {
                if persist_error.is_some() {
                    return;
                }
                if copilot_json_stream && chunk.stream == "stdout" {
                    return;
                }
                let remaining = MAX_PERSISTED_COMMAND_BYTES.saturating_sub(persisted_bytes);
                if remaining == 0 {
                    if !truncation_logged {
                        persist_error = self
                            .append_log(
                                &snapshot.run_id,
                                &command_id,
                                sequence,
                                "system",
                                "[Further command output omitted by Quorum's persisted log limit.]",
                                true,
                            )
                            .err();
                        sequence += 1;
                        truncation_logged = true;
                    }
                    return;
                }
                let retained = remaining.min(chunk.bytes.len());
                let text = String::from_utf8_lossy(&chunk.bytes[..retained]);
                if let Err(error) = self.append_log(
                    &snapshot.run_id,
                    &command_id,
                    sequence,
                    chunk.stream,
                    &text,
                    retained < chunk.bytes.len(),
                ) {
                    persist_error = Some(error);
                    return;
                }
                sequence += 1;
                persisted_bytes += retained;
                truncation_logged |= retained < chunk.bytes.len();
            };
            let result = self.runner.run(&request, control, &mut persist_chunk);
            if copilot_json_stream {
                if let Ok(result) = &result {
                    for message in copilot_log_messages(&result.stdout) {
                        persist_chunk(ProcessChunk {
                            stream: "stdout",
                            bytes: message.into_bytes(),
                        });
                    }
                }
            }
            result
        };
        let git_metadata_error = request
            .untrusted
            .then(|| validate_snapshot_git_metadata(snapshot))
            .transpose()
            .err();
        if let Some(error) = persist_error {
            return Err(WorkerError::database(error));
        }
        let completed_at = now();
        match result {
            Ok(result) => {
                let status = if control.cancelled() {
                    "cancelled"
                } else if result.success {
                    "succeeded"
                } else {
                    "failed"
                };
                self.store
                    .with_connection(|connection| {
                        connection.execute(
                            "UPDATE execution_commands
                             SET status = ?2, exit_code = ?3, output_truncated = ?4,
                                 completed_at = ?5
                             WHERE id = ?1",
                            params![
                                command_id,
                                status,
                                result.exit_code,
                                result.capture_truncated || truncation_logged,
                                completed_at
                            ],
                        )?;
                        Ok(())
                    })
                    .map_err(WorkerError::database)?;
                if let Some(error) = git_metadata_error {
                    return Err(error);
                }
                if control.cancelled() {
                    return Err(WorkerError::new("cancelled", "Execution was cancelled."));
                }
                Ok(CommandExecution {
                    id: command_id,
                    result,
                })
            }
            Err(error) => {
                let message = format!("Could not start or observe `{}`: {error}", request.program);
                let _ = self.append_log(
                    &snapshot.run_id,
                    &command_id,
                    sequence,
                    "system",
                    &message,
                    false,
                );
                self.store
                    .with_connection(|connection| {
                        connection.execute(
                            "UPDATE execution_commands
                             SET status = ?2, completed_at = ?3 WHERE id = ?1",
                            params![
                                command_id,
                                if control.cancelled() {
                                    "cancelled"
                                } else {
                                    "failed"
                                },
                                completed_at
                            ],
                        )?;
                        Ok(())
                    })
                    .map_err(WorkerError::database)?;
                if let Some(metadata_error) = git_metadata_error {
                    return Err(metadata_error);
                }
                Err(WorkerError::new("process_start_failed", message))
            }
        }
    }

    fn append_log(
        &self,
        run_id: &str,
        command_id: &str,
        sequence: usize,
        stream: &str,
        text: &str,
        truncated: bool,
    ) -> Result<(), AppError> {
        let text = if text.len() <= 16_384 {
            text.to_owned()
        } else {
            let mut boundary = 16_384;
            while !text.is_char_boundary(boundary) {
                boundary -= 1;
            }
            text[..boundary].to_owned()
        };
        self.store.with_connection(|connection| {
            connection.execute(
                "INSERT INTO execution_logs (
                   id, run_id, command_id, sequence, stream, text, truncated, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    Uuid::new_v4().to_string(),
                    run_id,
                    command_id,
                    sequence,
                    stream,
                    text,
                    truncated,
                    now()
                ],
            )?;
            Ok(())
        })
    }

    fn mark_blocked(&self, run_id: &str, code: &str, message: &str) -> Result<(), AppError> {
        let timestamp = now();
        self.store.with_connection(|connection| {
            let transaction = connection.unchecked_transaction()?;
            transaction.execute(
                "UPDATE execution_runs
                 SET status = 'blocked', error_code = ?2, error_message = ?3,
                     updated_at = ?4, completed_at = ?4
                 WHERE run_id = ?1 AND status <> 'cancelled'",
                params![run_id, code, message, timestamp],
            )?;
            transaction.execute(
                "UPDATE runs SET outcome = 'blocked', updated_at = ?2 WHERE id = ?1",
                params![run_id, timestamp],
            )?;
            transaction.execute(
                "UPDATE execution_attempts
                 SET status = 'blocked', error_code = ?2, error_message = ?3,
                     completed_at = ?4
                 WHERE run_id = ?1 AND status = 'running'",
                params![run_id, code, message, timestamp],
            )?;
            append_event(
                &transaction,
                run_id,
                "execution_blocked",
                &json!({"code": code, "message": message}),
                &timestamp,
            )?;
            transaction.commit()?;
            Ok(())
        })
    }

    fn mark_cancelled(&self, run_id: &str) -> Result<(), AppError> {
        let timestamp = now();
        self.store.with_connection(|connection| {
            let transaction = connection.unchecked_transaction()?;
            transaction.execute(
                "UPDATE execution_commands
                 SET status = 'cancelled', completed_at = ?2
                 WHERE run_id = ?1 AND status = 'running'",
                params![run_id, timestamp],
            )?;
            transaction.execute(
                "UPDATE execution_attempts
                 SET status = 'cancelled', error_code = NULL, error_message = NULL,
                     completed_at = ?2
                 WHERE run_id = ?1 AND status = 'running'",
                params![run_id, timestamp],
            )?;
            transaction.execute(
                "UPDATE execution_runs
                 SET status = 'cancelled', error_code = NULL, error_message = NULL,
                     updated_at = ?2, completed_at = ?2
                 WHERE run_id = ?1",
                params![run_id, timestamp],
            )?;
            transaction.execute(
                "UPDATE runs SET outcome = 'cancelled', updated_at = ?2 WHERE id = ?1",
                params![run_id, timestamp],
            )?;
            append_event(
                &transaction,
                run_id,
                "execution_cancelled",
                &json!({"message": "Only this run's owned process group was terminated."}),
                &timestamp,
            )?;
            transaction.commit()?;
            Ok(())
        })
    }
}

fn builder_arguments(snapshot: &WorkerSnapshot, prompt: &str) -> Vec<String> {
    let mut arguments = copilot_common_arguments(&snapshot.worktree_path, false);
    if snapshot.builder_session_state == "resumable" {
        arguments.push(format!("--resume={}", snapshot.builder_session_name));
    } else {
        arguments.extend([
            "--model".to_owned(),
            snapshot.builder_model.clone(),
            "--session-id".to_owned(),
            snapshot.builder_session_id.clone(),
            "--name".to_owned(),
            snapshot.builder_session_name.clone(),
        ]);
    }
    arguments.extend(["-p".to_owned(), prompt.to_owned()]);
    arguments
}

fn reviewer_arguments(snapshot: &WorkerSnapshot, prompt: &str) -> Vec<String> {
    let mut arguments = copilot_common_arguments(&snapshot.worktree_path, true);
    if snapshot.reviewer_session_state == "resumable" {
        arguments.push(format!("--resume={}", snapshot.reviewer_session_name));
    } else {
        arguments.extend([
            "--model".to_owned(),
            snapshot.reviewer_model.clone(),
            "--session-id".to_owned(),
            snapshot.reviewer_session_id.clone(),
            "--name".to_owned(),
            snapshot.reviewer_session_name.clone(),
        ]);
    }
    arguments.extend(["-p".to_owned(), prompt.to_owned()]);
    arguments
}

fn copilot_common_arguments(worktree_path: &Path, reviewer: bool) -> Vec<String> {
    let mut arguments = vec![
        "-C".to_owned(),
        worktree_path.to_string_lossy().into_owned(),
        "--output-format".to_owned(),
        "json".to_owned(),
        "--stream=on".to_owned(),
        "--silent".to_owned(),
        "--no-ask-user".to_owned(),
        "--no-custom-instructions".to_owned(),
        "--disable-builtin-mcps".to_owned(),
        "--disallow-temp-dir".to_owned(),
        "--allow-all-tools".to_owned(),
        "--no-auto-update".to_owned(),
        "--no-bash-env".to_owned(),
        "--secret-env-vars=COPILOT_GITHUB_TOKEN,GH_TOKEN,GITHUB_TOKEN".to_owned(),
        "--no-remote-export".to_owned(),
    ];
    if reviewer {
        arguments.extend([
            "--plan".to_owned(),
            "--deny-tool=write".to_owned(),
            "--deny-tool=shell".to_owned(),
        ]);
    }
    arguments
}

fn copilot_process_request(
    snapshot: &WorkerSnapshot,
    arguments: Vec<String>,
    role: &str,
) -> Result<ProcessRequest, WorkerError> {
    validate_confinement_tree(&snapshot.worktree_path)?;
    let runtime = secure_runtime_directory(&snapshot.worktree_path, role)?;
    let logs = runtime.join("logs");
    let runtime_root = fs::canonicalize(&runtime).map_err(|error| {
        WorkerError::new(
            "sandbox_unavailable",
            format!("Could not resolve the confined Copilot runtime: {error}"),
        )
    })?;
    secure_directory(&logs, &runtime_root)?;
    let mut arguments = arguments;
    arguments.extend(["--log-dir".to_owned(), logs.to_string_lossy().into_owned()]);
    let environment = copilot_environment(&runtime);
    for (_, path) in &environment {
        secure_directory(path, &runtime_root)?;
    }
    #[cfg(test)]
    let mut request = ProcessRequest::new(
        snapshot.copilot_program.clone(),
        arguments,
        snapshot.worktree_path.clone(),
    );
    #[cfg(all(not(test), target_os = "macos"))]
    let mut request = {
        let profile = macos_sandbox_profile(&snapshot.worktree_path)?;
        let mut sandbox_arguments =
            vec!["-p".to_owned(), profile, snapshot.copilot_program.clone()];
        sandbox_arguments.extend(arguments);
        ProcessRequest::new(
            "/usr/bin/sandbox-exec".to_owned(),
            sandbox_arguments,
            snapshot.worktree_path.clone(),
        )
    };
    #[cfg(all(not(test), not(target_os = "macos")))]
    return Err(WorkerError::new(
        "sandbox_unavailable",
        "No whole-process Copilot confinement backend is configured for this platform.",
    ));
    request.environment = environment
        .into_iter()
        .map(|(name, path)| (name.to_owned(), path.to_string_lossy().into_owned()))
        .collect();
    #[cfg(not(test))]
    request
        .environment
        .push(copilot_authentication_environment()?);
    request.untrusted = true;
    Ok(request)
}

#[cfg(not(test))]
fn copilot_authentication_environment() -> Result<(String, String), WorkerError> {
    if let Some(token) = inherited_copilot_token(|name| std::env::var(name).ok()) {
        return Ok(("COPILOT_GITHUB_TOKEN".to_owned(), token));
    }
    let gh = resolve_executable("gh").ok_or_else(|| {
        WorkerError::new(
            "copilot_auth",
            "Copilot authentication is unavailable because GitHub CLI was not found on PATH. Install `gh`, run `gh auth login`, and resume execution.",
        )
    })?;
    let output = Command::new(gh)
        .args(["auth", "token"])
        .output()
        .map_err(|error| {
            WorkerError::new(
                "copilot_auth",
                format!("GitHub CLI could not provide Copilot authentication: {error}"),
            )
        })?;
    let token = String::from_utf8(output.stdout)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let Some(token) = token.filter(|_| output.status.success()) else {
        return Err(WorkerError::new(
            "copilot_auth",
            "GitHub CLI has no usable authentication token. Run `gh auth login`, then resume execution.",
        ));
    };
    Ok(("COPILOT_GITHUB_TOKEN".to_owned(), token))
}

fn inherited_copilot_token(mut lookup: impl FnMut(&str) -> Option<String>) -> Option<String> {
    ["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"]
        .into_iter()
        .find_map(|name| lookup(name).filter(|value| !value.trim().is_empty()))
}

fn copilot_environment(runtime: &Path) -> [(&'static str, PathBuf); 7] {
    let copilot_home = runtime.join("copilot-home");
    [
        ("HOME", copilot_home.clone()),
        ("COPILOT_HOME", copilot_home),
        ("TMPDIR", runtime.join("tmp")),
        ("XDG_CACHE_HOME", runtime.join("xdg-cache")),
        ("XDG_CONFIG_HOME", runtime.join("xdg-config")),
        ("XDG_DATA_HOME", runtime.join("xdg-data")),
        ("XDG_STATE_HOME", runtime.join("xdg-state")),
    ]
}

fn verification_process_request(snapshot: &WorkerSnapshot) -> Result<ProcessRequest, WorkerError> {
    validate_confinement_tree(&snapshot.worktree_path)?;
    let runtime = secure_runtime_directory(&snapshot.worktree_path, "verification")?;
    let runtime_root = fs::canonicalize(&runtime).map_err(|error| {
        WorkerError::new(
            "sandbox_unavailable",
            format!("Could not resolve the confined verification runtime: {error}"),
        )
    })?;
    let environment = [
        ("TMPDIR", runtime.join("tmp")),
        ("XDG_CACHE_HOME", runtime.join("xdg-cache")),
        ("XDG_CONFIG_HOME", runtime.join("xdg-config")),
        ("XDG_DATA_HOME", runtime.join("xdg-data")),
        ("XDG_STATE_HOME", runtime.join("xdg-state")),
    ];
    for (_, path) in &environment {
        secure_directory(path, &runtime_root)?;
    }
    #[cfg(test)]
    let mut request = ProcessRequest::new(
        snapshot.verification_program.clone(),
        snapshot.verification_arguments.clone(),
        snapshot.worktree_path.clone(),
    );
    #[cfg(all(not(test), target_os = "macos"))]
    let mut request = {
        let profile = macos_sandbox_profile(&snapshot.worktree_path)?;
        let mut arguments = vec![
            "-p".to_owned(),
            profile,
            snapshot.verification_program.clone(),
        ];
        arguments.extend(snapshot.verification_arguments.clone());
        ProcessRequest::new(
            "/usr/bin/sandbox-exec".to_owned(),
            arguments,
            snapshot.worktree_path.clone(),
        )
    };
    #[cfg(all(not(test), not(target_os = "macos")))]
    return Err(WorkerError::new(
        "sandbox_unavailable",
        "No whole-process verification confinement backend is configured for this platform.",
    ));
    request.environment = environment
        .into_iter()
        .map(|(name, path)| (name.to_owned(), path.to_string_lossy().into_owned()))
        .collect();
    request.untrusted = true;
    Ok(request)
}

#[cfg(unix)]
fn validate_confinement_tree(worktree_path: &Path) -> Result<(), WorkerError> {
    use std::os::unix::fs::MetadataExt;

    let root = fs::canonicalize(worktree_path).map_err(|error| {
        WorkerError::new(
            "sandbox_unavailable",
            format!("Could not resolve the managed worktree for confinement: {error}"),
        )
    })?;
    let mut pending = vec![root];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(|error| {
            WorkerError::new(
                "sandbox_unavailable",
                format!(
                    "Could not inspect {} for confinement: {error}",
                    directory.display()
                ),
            )
        })? {
            let entry = entry.map_err(|error| {
                WorkerError::new(
                    "sandbox_unavailable",
                    format!("Could not inspect a worktree entry: {error}"),
                )
            })?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                WorkerError::new(
                    "sandbox_unavailable",
                    format!(
                        "Could not inspect {} for confinement: {error}",
                        path.display()
                    ),
                )
            })?;
            let file_type = metadata.file_type();
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file() {
                if metadata.nlink() != 1 {
                    return Err(WorkerError::new(
                        "sandbox_unavailable",
                        format!(
                            "Quorum refuses to launch confined code while {} has multiple hard links; writing it could modify a file outside the managed worktree.",
                            path.display()
                        ),
                    ));
                }
            } else if !file_type.is_symlink() {
                return Err(WorkerError::new(
                    "sandbox_unavailable",
                    format!(
                        "Quorum refuses to launch confined code while {} is a non-regular special file.",
                        path.display()
                    ),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_confinement_tree(_worktree_path: &Path) -> Result<(), WorkerError> {
    Err(WorkerError::new(
        "sandbox_unavailable",
        "Quorum cannot verify hard-link confinement on this platform.",
    ))
}

fn secure_runtime_directory(worktree_path: &Path, role: &str) -> Result<PathBuf, WorkerError> {
    let worktree = fs::canonicalize(worktree_path).map_err(|error| {
        WorkerError::new(
            "sandbox_unavailable",
            format!("Could not resolve the managed worktree for confinement: {error}"),
        )
    })?;
    let root = worktree.join(RUNTIME_DIRECTORY);
    secure_directory(&root, &worktree)?;
    let role = root.join(role);
    secure_directory(&role, &worktree)?;
    Ok(role)
}

fn secure_directory(path: &Path, root: &Path) -> Result<(), WorkerError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => {
            return Err(WorkerError::new(
                "sandbox_unavailable",
                format!(
                    "Confined runtime path {} is not a real directory.",
                    path.display()
                ),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|error| {
                WorkerError::new(
                    "sandbox_unavailable",
                    format!(
                        "Could not create confined runtime {}: {error}",
                        path.display()
                    ),
                )
            })?;
        }
        Err(error) => {
            return Err(WorkerError::new(
                "sandbox_unavailable",
                format!("Could not inspect {}: {error}", path.display()),
            ));
        }
    }
    let resolved = fs::canonicalize(path).map_err(|error| {
        WorkerError::new(
            "sandbox_unavailable",
            format!("Could not resolve {}: {error}", path.display()),
        )
    })?;
    if !resolved.starts_with(root) {
        return Err(WorkerError::new(
            "sandbox_unavailable",
            format!(
                "Confined runtime {} escapes the managed worktree.",
                path.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
#[cfg_attr(test, allow(dead_code))]
fn macos_sandbox_profile(worktree_path: &Path) -> Result<String, WorkerError> {
    let worktree = fs::canonicalize(worktree_path).map_err(|error| {
        WorkerError::new(
            "sandbox_unavailable",
            format!("Could not resolve the managed worktree for confinement: {error}"),
        )
    })?;
    let escaped = worktree
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let escaped_git = worktree
        .join(".git")
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    Ok(format!(
        "(version 1)(allow default)(deny file-write*)(allow file-write* (subpath \"{escaped}\"))\
         (deny file-write* (literal \"{escaped_git}\") (subpath \"{escaped_git}\"))"
    ))
}

fn validate_owned_branch(
    repository_path: &str,
    branch_name: &str,
    base_commit: &str,
) -> Result<(), WorkerError> {
    let git = resolve_executable("git").ok_or_else(|| {
        WorkerError::new(
            "missing_git",
            "Git executable `git` is no longer available on PATH.",
        )
    })?;
    let reference = format!("refs/heads/{branch_name}");
    let exists = direct_output(
        &git,
        repository_path,
        &["show-ref", "--verify", "--quiet", &reference],
    )
    .map_err(|error| {
        WorkerError::new(
            "worktree_ownership_conflict",
            format!("Git could not inspect the claimed branch: {error}"),
        )
    })?;
    if !exists.status.success() {
        return Ok(());
    }
    let actual = direct_output(
        &git,
        repository_path,
        &["rev-parse", "--verify", &reference],
    )
    .map_err(|error| {
        WorkerError::new(
            "worktree_ownership_conflict",
            format!("Git could not resolve the claimed branch: {error}"),
        )
    })?;
    if !actual.status.success() || String::from_utf8_lossy(&actual.stdout).trim() != base_commit {
        return Err(WorkerError::new(
            "worktree_ownership_conflict",
            format!(
                "The claimed branch {branch_name} no longer points at the persisted base commit. Quorum left it unchanged."
            ),
        ));
    }
    Ok(())
}

fn trusted_git_output(
    git: &str,
    worktree_path: &Path,
    identity: &GitMetadataIdentity,
    arguments: &[&str],
) -> io::Result<std::process::Output> {
    let mut command = Command::new(git);
    clear_git_environment(&mut command);
    command
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
        ])
        .arg(format!("--git-dir={}", identity.git_dir.path))
        .arg(format!("--work-tree={}", worktree_path.display()))
        .args(arguments)
        .current_dir(worktree_path)
        .env("GIT_COMMON_DIR", &identity.common_dir.path)
        .env("GIT_CONFIG", "/dev/null")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
}

#[allow(clippy::too_many_lines)]
fn resolve_git_metadata_identity(worktree_path: &Path) -> Result<GitMetadataIdentity, WorkerError> {
    let worktree = fs::canonicalize(worktree_path).map_err(|error| {
        WorkerError::new(
            "git_metadata_changed",
            format!("Could not resolve the managed worktree: {error}"),
        )
    })?;
    let dot_git = worktree.join(".git");
    let metadata = fs::symlink_metadata(&dot_git).map_err(|error| {
        WorkerError::new(
            "git_metadata_changed",
            format!("Could not inspect {}: {error}", dot_git.display()),
        )
    })?;
    let git_dir = if metadata.file_type().is_dir() {
        fs::canonicalize(&dot_git)
    } else if metadata.file_type().is_file() {
        let contents = read_small_regular_file(&dot_git, 4096, "managed .git pointer")?;
        let pointer = std::str::from_utf8(&contents).map_err(|error| {
            WorkerError::new(
                "git_metadata_changed",
                format!("The managed .git pointer is not valid UTF-8: {error}"),
            )
        })?;
        let target = pointer
            .trim()
            .strip_prefix("gitdir:")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                WorkerError::new(
                    "git_metadata_changed",
                    "The managed .git pointer is malformed.",
                )
            })?;
        let target = Path::new(target);
        fs::canonicalize(if target.is_absolute() {
            target.to_path_buf()
        } else {
            worktree.join(target)
        })
    } else {
        return Err(WorkerError::new(
            "git_metadata_changed",
            "The managed .git entry is not a real file or directory.",
        ));
    }
    .map_err(|error| {
        WorkerError::new(
            "git_metadata_changed",
            format!("Could not resolve the managed Git directory: {error}"),
        )
    })?;
    if !git_dir.is_dir() {
        return Err(WorkerError::new(
            "git_metadata_changed",
            "The managed Git directory is not a directory.",
        ));
    }
    let common_pointer = git_dir.join("commondir");
    let common_dir = match fs::symlink_metadata(&common_pointer) {
        Ok(pointer_metadata) if pointer_metadata.file_type().is_file() => {
            let contents =
                read_small_regular_file(&common_pointer, 4096, "Git common-directory pointer")?;
            let target = std::str::from_utf8(&contents)
                .map_err(|error| {
                    WorkerError::new(
                        "git_metadata_changed",
                        format!("The Git common-directory pointer is not valid UTF-8: {error}"),
                    )
                })?
                .trim();
            if target.is_empty() {
                return Err(WorkerError::new(
                    "git_metadata_changed",
                    "The Git common-directory pointer is empty.",
                ));
            }
            let target = Path::new(target);
            fs::canonicalize(if target.is_absolute() {
                target.to_path_buf()
            } else {
                git_dir.join(target)
            })
            .map_err(|error| {
                WorkerError::new(
                    "git_metadata_changed",
                    format!("Could not resolve the Git common directory: {error}"),
                )
            })?
        }
        Ok(_) => {
            return Err(WorkerError::new(
                "git_metadata_changed",
                "The Git common-directory pointer is not a regular file.",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => git_dir.clone(),
        Err(error) => {
            return Err(WorkerError::new(
                "git_metadata_changed",
                format!("Could not inspect the Git common-directory pointer: {error}"),
            ));
        }
    };
    if !common_dir.is_dir() {
        return Err(WorkerError::new(
            "git_metadata_changed",
            "The Git common directory is not a directory.",
        ));
    }
    Ok(GitMetadataIdentity {
        git_dir: filesystem_identity(&git_dir)?,
        common_dir: filesystem_identity(&common_dir)?,
    })
}

fn read_small_regular_file(
    path: &Path,
    limit: usize,
    description: &str,
) -> Result<Vec<u8>, WorkerError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        WorkerError::new(
            "git_metadata_changed",
            format!(
                "Could not inspect {description} {}: {error}",
                path.display()
            ),
        )
    })?;
    if !metadata.file_type().is_file() {
        return Err(WorkerError::new(
            "git_metadata_changed",
            format!("{description} {} is not a regular file.", path.display()),
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path).map_err(|error| {
        WorkerError::new(
            "git_metadata_changed",
            format!(
                "Could not safely open {description} {}: {error}",
                path.display()
            ),
        )
    })?;
    let mut contents = Vec::new();
    Read::by_ref(&mut file)
        .take(u64::try_from(limit).unwrap_or(u64::MAX) + 1)
        .read_to_end(&mut contents)
        .map_err(|error| {
            WorkerError::new(
                "git_metadata_changed",
                format!("Could not read {description} {}: {error}", path.display()),
            )
        })?;
    if contents.len() > limit {
        return Err(WorkerError::new(
            "git_metadata_changed",
            format!(
                "{description} {} exceeds its safe size bound.",
                path.display()
            ),
        ));
    }
    Ok(contents)
}

fn filesystem_identity(path: &Path) -> Result<FilesystemIdentity, WorkerError> {
    let canonical = fs::canonicalize(path).map_err(|error| {
        WorkerError::new(
            "git_metadata_changed",
            format!("Could not resolve Git metadata {}: {error}", path.display()),
        )
    })?;
    let metadata = fs::metadata(&canonical).map_err(|error| {
        WorkerError::new(
            "git_metadata_changed",
            format!(
                "Could not inspect Git metadata {}: {error}",
                canonical.display()
            ),
        )
    })?;
    #[cfg(unix)]
    let (device, inode) = {
        use std::os::unix::fs::MetadataExt;
        (Some(metadata.dev()), Some(metadata.ino()))
    };
    #[cfg(not(unix))]
    let (device, inode) = (None, None);
    Ok(FilesystemIdentity {
        path: canonical.to_string_lossy().into_owned(),
        device,
        inode,
    })
}

fn validate_snapshot_git_metadata(snapshot: &WorkerSnapshot) -> Result<(), WorkerError> {
    let expected = snapshot.git_metadata.as_ref().ok_or_else(|| {
        WorkerError::new(
            "git_metadata_changed",
            "The managed worktree has no persisted trusted Git metadata identity.",
        )
    })?;
    let current = resolve_git_metadata_identity(&snapshot.worktree_path)?;
    if &current != expected {
        return Err(WorkerError::new(
            "git_metadata_changed",
            "The managed worktree Git directory or common-directory identity changed during an untrusted command.",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn validate_owned_worktree(
    repository_path: &str,
    branch_name: &str,
    worktree_path: &Path,
    expected_identity: Option<&GitMetadataIdentity>,
) -> Result<GitMetadataIdentity, WorkerError> {
    let git = resolve_executable("git").ok_or_else(|| {
        WorkerError::new(
            "missing_git",
            "Git executable `git` is no longer available on PATH.",
        )
    })?;
    let identity = resolve_git_metadata_identity(worktree_path)?;
    if expected_identity.is_some_and(|expected| expected != &identity) {
        return Err(WorkerError::new(
            "git_metadata_changed",
            "The managed worktree Git directory or common-directory identity changed after Quorum persisted ownership.",
        ));
    }
    let root = trusted_git_output(
        &git,
        worktree_path,
        &identity,
        &["rev-parse", "--show-toplevel"],
    )
    .map_err(|error| {
        WorkerError::new(
            "invalid_managed_worktree",
            format!("Git could not inspect the managed worktree: {error}"),
        )
    })?;
    let actual_root = String::from_utf8_lossy(&root.stdout).trim().to_owned();
    if !root.status.success()
        || fs::canonicalize(actual_root).ok() != fs::canonicalize(worktree_path).ok()
    {
        return Err(WorkerError::new(
            "worktree_path_conflict",
            "The claimed managed path resolves to a different Git checkout.",
        ));
    }
    let branch = trusted_git_output(
        &git,
        worktree_path,
        &identity,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
    )
    .map_err(|error| {
        WorkerError::new(
            "worktree_branch_conflict",
            format!("Git could not inspect the managed branch: {error}"),
        )
    })?;
    if !branch.status.success() || String::from_utf8_lossy(&branch.stdout).trim() != branch_name {
        return Err(WorkerError::new(
            "worktree_branch_conflict",
            format!("The claimed worktree is not attached to branch {branch_name}."),
        ));
    }
    let registration = direct_output(&git, repository_path, &["worktree", "list", "--porcelain"])
        .map_err(|error| {
        WorkerError::new(
            "worktree_ownership_conflict",
            format!("Git could not inspect worktree registrations: {error}"),
        )
    })?;
    let expected_root = fs::canonicalize(worktree_path).map_err(|error| {
        WorkerError::new(
            "worktree_ownership_conflict",
            format!("Could not resolve the managed worktree: {error}"),
        )
    })?;
    let expected_branch = format!("refs/heads/{branch_name}");
    let registered = String::from_utf8_lossy(&registration.stdout)
        .split("\n\n")
        .any(|entry| {
            let mut path_matches = false;
            let mut branch_matches = false;
            for line in entry.lines() {
                if let Some(path) = line.strip_prefix("worktree ") {
                    path_matches = fs::canonicalize(path).ok().as_ref() == Some(&expected_root);
                } else if let Some(branch) = line.strip_prefix("branch ") {
                    branch_matches = branch == expected_branch;
                }
            }
            path_matches && branch_matches
        });
    if !registration.status.success() || !registered {
        return Err(WorkerError::new(
            "worktree_ownership_conflict",
            "Git no longer registers the claimed branch at the persisted managed worktree path.",
        ));
    }
    let conflicts = trusted_git_output(
        &git,
        worktree_path,
        &identity,
        &["diff", "--name-only", "--diff-filter=U"],
    )
    .map_err(|error| {
        WorkerError::new(
            "worktree_conflict",
            format!("Git could not inspect worktree conflicts: {error}"),
        )
    })?;
    if !conflicts.status.success() || !conflicts.stdout.is_empty() {
        return Err(WorkerError::new(
            "worktree_conflict",
            "The managed worktree contains unresolved conflicts. Resolve them explicitly before resuming; Quorum will not reset or clean it.",
        ));
    }
    let reserved = trusted_git_output(
        &git,
        worktree_path,
        &identity,
        &["ls-files", "--", RUNTIME_DIRECTORY],
    )
    .map_err(|error| {
        WorkerError::new(
            "sandbox_unavailable",
            format!("Git could not inspect Quorum's reserved runtime path: {error}"),
        )
    })?;
    if !reserved.status.success() || !reserved.stdout.is_empty() {
        return Err(WorkerError::new(
            "sandbox_unavailable",
            format!(
                "The repository tracks Quorum's reserved {RUNTIME_DIRECTORY} path, so a confined Copilot runtime cannot be established."
            ),
        ));
    }
    Ok(identity)
}

fn validate_incomplete_owned_worktree(
    repository_path: &str,
    branch_name: &str,
    worktree_path: &Path,
    base_commit: &str,
) -> Result<GitMetadataIdentity, WorkerError> {
    validate_owned_branch(repository_path, branch_name, base_commit)?;
    let identity = validate_owned_worktree(repository_path, branch_name, worktree_path, None)?;
    let source_identity = resolve_git_metadata_identity(Path::new(repository_path))?;
    if identity.common_dir != source_identity.common_dir {
        return Err(WorkerError::new(
            "worktree_ownership_conflict",
            "The incomplete managed worktree does not use the source repository's exact Git common directory.",
        ));
    }
    let git = resolve_executable("git").ok_or_else(|| {
        WorkerError::new(
            "missing_git",
            "Git executable `git` is no longer available on PATH.",
        )
    })?;
    let head = trusted_git_output(
        &git,
        worktree_path,
        &identity,
        &["rev-parse", "--verify", "HEAD"],
    )
    .map_err(|error| {
        WorkerError::new(
            "worktree_ownership_conflict",
            format!("Git could not inspect the incomplete managed worktree HEAD: {error}"),
        )
    })?;
    if !head.status.success() || String::from_utf8_lossy(&head.stdout).trim() != base_commit {
        return Err(WorkerError::new(
            "worktree_ownership_conflict",
            "The incomplete managed worktree is not at the persisted base commit.",
        ));
    }
    let status = trusted_git_output(
        &git,
        worktree_path,
        &identity,
        &[
            "status",
            "--porcelain=v1",
            "--untracked-files=normal",
            "--",
            ".",
            &format!(":(exclude){RUNTIME_DIRECTORY}/**"),
        ],
    )
    .map_err(|error| {
        WorkerError::new(
            "worktree_ownership_conflict",
            format!("Git could not inspect the incomplete managed worktree content: {error}"),
        )
    })?;
    if !status.status.success() || !status.stdout.is_empty() {
        return Err(WorkerError::new(
            "worktree_ownership_conflict",
            "The incomplete managed worktree contains content that cannot be attributed to Quorum's worktree-creation stage.",
        ));
    }
    Ok(identity)
}

fn open_regular_contained(
    path: &Path,
    canonical_root: &Path,
    description: &str,
) -> Result<File, WorkerError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        WorkerError::new(
            "unsafe_review_file",
            format!(
                "Could not inspect {description} {}: {error}",
                path.display()
            ),
        )
    })?;
    if !metadata.file_type().is_file() {
        return Err(WorkerError::new(
            "unsafe_review_file",
            format!(
                "Quorum refuses to read {description} {} because it is a symlink or non-regular file.",
                path.display()
            ),
        ));
    }
    let resolved = fs::canonicalize(path).map_err(|error| {
        WorkerError::new(
            "unsafe_review_file",
            format!(
                "Could not resolve {description} {}: {error}",
                path.display()
            ),
        )
    })?;
    if !resolved.starts_with(canonical_root) {
        return Err(WorkerError::new(
            "unsafe_review_file",
            format!("{description} {} escapes its managed root.", path.display()),
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(|error| {
        WorkerError::new(
            "unsafe_review_file",
            format!(
                "Could not safely open {description} {}: {error}",
                path.display()
            ),
        )
    })?;
    if !file
        .metadata()
        .map_err(|error| {
            WorkerError::new(
                "unsafe_review_file",
                format!("Could not inspect opened {description}: {error}"),
            )
        })?
        .is_file()
        || fs::canonicalize(path).ok().as_ref() != Some(&resolved)
    {
        return Err(WorkerError::new(
            "unsafe_review_file",
            format!(
                "{description} {} changed while being opened.",
                path.display()
            ),
        ));
    }
    Ok(file)
}

#[allow(clippy::too_many_lines)]
fn run_trusted_git_capture(
    git: &str,
    worktree_path: &Path,
    identity: &GitMetadataIdentity,
    arguments: &[&str],
    budget: &mut EvidenceBudget,
    cancellation: Option<&RunControl>,
    stdout_limit: Option<usize>,
) -> Result<Vec<u8>, WorkerError> {
    if &resolve_git_metadata_identity(worktree_path)? != identity {
        return Err(WorkerError::new(
            "git_metadata_changed",
            "The managed worktree Git metadata identity changed before evidence capture.",
        ));
    }
    budget.check(cancellation)?;
    let local_control = RunControl::default();
    let control = cancellation.unwrap_or(&local_control);
    let mut command = Command::new(git);
    clear_git_environment(&mut command);
    command
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
        ])
        .arg(format!("--git-dir={}", identity.git_dir.path))
        .arg(format!("--work-tree={}", worktree_path.display()))
        .args(arguments)
        .current_dir(worktree_path)
        .env("GIT_COMMON_DIR", &identity.common_dir.path)
        .env("GIT_CONFIG", "/dev/null")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|error| {
        WorkerError::new(
            "diff_failed",
            format!("Could not start trusted Git evidence capture: {error}"),
        )
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| WorkerError::new("diff_failed", "Could not capture trusted Git stdout."))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| WorkerError::new("diff_failed", "Could not capture trusted Git stderr."))?;
    let (sender, receiver) = mpsc::sync_channel(PROCESS_OUTPUT_CHANNEL_CAPACITY);
    let stdout_reader = spawn_reader(stdout, "stdout", sender.clone());
    let stderr_reader = spawn_reader(stderr, "stderr", sender);
    control.install_child(child).map_err(|error| {
        WorkerError::new(
            "diff_failed",
            format!("Could not own trusted Git evidence capture: {error}"),
        )
    })?;
    let mut child_guard = InstalledChildGuard::new(control);
    let mut captured_stdout = Vec::new();
    let mut captured_stderr = Vec::new();
    let mut status = None;
    let mut readers_finished = false;
    loop {
        budget.check(cancellation)?;
        match receiver.recv_timeout(Duration::from_millis(20)) {
            Ok(chunk) => {
                budget.consume(chunk.bytes.len(), cancellation)?;
                if chunk.stream == "stderr" {
                    captured_stderr.extend_from_slice(&chunk.bytes);
                } else {
                    if stdout_limit.is_some_and(|limit| {
                        captured_stdout.len().saturating_add(chunk.bytes.len()) > limit
                    }) {
                        return Err(review_evidence_too_large());
                    }
                    captured_stdout.extend_from_slice(&chunk.bytes);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => readers_finished = true,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if status.is_none() {
            if let Some(completed) = control.try_wait().map_err(|error| {
                WorkerError::new(
                    "diff_failed",
                    format!("Could not observe trusted Git evidence capture: {error}"),
                )
            })? {
                control.finish_child();
                child_guard.disarm();
                status = Some(completed);
            }
        }
        if status.is_some() && readers_finished {
            break;
        }
    }
    let _ = stdout_reader.join();
    let _ = stderr_reader.join();
    let status = status.expect("trusted Git child completed before its output readers");
    if !status.success() {
        return Err(WorkerError::new(
            "diff_failed",
            command_diagnostic(
                "Git could not produce complete trusted diff evidence.",
                &captured_stderr,
            ),
        ));
    }
    if &resolve_git_metadata_identity(worktree_path)? != identity {
        return Err(WorkerError::new(
            "git_metadata_changed",
            "The managed worktree Git metadata identity changed during evidence capture.",
        ));
    }
    Ok(captured_stdout)
}

#[cfg(test)]
fn collect_base_evidence(
    worktree_path: &Path,
    tracked_path: &Path,
    untracked_path: &Path,
) -> Result<BaseEvidence, WorkerError> {
    let root = fs::canonicalize(worktree_path).map_err(|error| {
        WorkerError::new(
            "diff_failed",
            format!("Could not resolve the managed worktree: {error}"),
        )
    })?;
    let mut budget = EvidenceBudget::new();
    let tracked =
        read_complete_evidence_file(tracked_path, &root, "tracked diff", &mut budget, None)?;
    let untracked = read_complete_evidence_file(
        untracked_path,
        &root,
        "untracked path list",
        &mut budget,
        None,
    )?;
    collect_base_evidence_bytes(worktree_path, &tracked, &untracked, &mut budget, None)
}

fn collect_base_evidence_bytes(
    worktree_path: &Path,
    tracked: &[u8],
    untracked: &[u8],
    budget: &mut EvidenceBudget,
    control: Option<&RunControl>,
) -> Result<BaseEvidence, WorkerError> {
    let root = fs::canonicalize(worktree_path).map_err(|error| {
        WorkerError::new(
            "diff_failed",
            format!("Could not resolve the managed worktree: {error}"),
        )
    })?;
    let mut hasher = StableHasher::new();
    budget.consume(tracked.len(), control)?;
    let tracked_length = u64::try_from(tracked.len()).map_err(|_| {
        WorkerError::new(
            "evidence_limit_exceeded",
            "Tracked evidence length cannot be represented canonically.",
        )
    })?;
    hasher.begin_record(b"tracked-diff");
    hasher.field(b"type", b"git-binary-diff");
    hasher.field_u64(b"byte-length", tracked_length);
    hasher.field(b"content", tracked);
    hasher.end_record();
    let mut review = Vec::new();
    append_review(&mut review, tracked)?;
    let mut untracked_paths = parse_nul_paths(untracked, budget, control)?;
    untracked_paths.sort_unstable();
    for relative in untracked_paths {
        let relative_path = Path::new(&relative);
        if relative_path.is_absolute()
            || relative_path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
            || relative_path.starts_with(RUNTIME_DIRECTORY)
        {
            return Err(WorkerError::new(
                "unsafe_review_file",
                format!("Git returned an unsafe untracked path: {relative:?}."),
            ));
        }
        let path = worktree_path.join(relative_path);
        let file = open_regular_contained(&path, &root, "untracked review file")?;
        let mode = review_file_mode(&file)?;
        let file_length_u64 = file
            .metadata()
            .map_err(|error| {
                WorkerError::new(
                    "diff_failed",
                    format!("Could not inspect an untracked review file: {error}"),
                )
            })?
            .len();
        hasher.begin_record(b"untracked-file");
        hasher.field(b"path", relative.as_bytes());
        hasher.field_u32(b"mode-type", mode);
        hasher.field_u64(b"byte-length", file_length_u64);
        hasher.begin_stream_field(b"content", file_length_u64);
        let header = format!(
            "\nnew file mode {mode:o}\n--- /dev/null\n+++ b/{relative}\n@@ untracked regular file @@\n"
        );
        let file_length = usize::try_from(file_length_u64).unwrap_or(usize::MAX);
        if review
            .len()
            .saturating_add(header.len())
            .saturating_add(file_length)
            .saturating_add(1)
            > MAX_REVIEW_DIFF_BYTES
        {
            return Err(review_evidence_too_large());
        }
        let mut content = Vec::new();
        stream_evidence_file(
            file,
            file_length_u64,
            &mut hasher,
            &mut content,
            budget,
            control,
        )?;
        hasher.end_record();
        let rendered = match String::from_utf8(content) {
            Ok(text) => text,
            Err(error) => hex_encode(error.as_bytes()),
        };
        append_review(&mut review, header.as_bytes())?;
        append_review(&mut review, rendered.as_bytes())?;
        append_review(&mut review, b"\n")?;
    }
    budget.check(control)?;
    let review_diff = String::from_utf8(review).map_err(|error| {
        WorkerError::new(
            "review_contract",
            format!("Complete Git review evidence is not valid UTF-8: {error}"),
        )
    })?;
    Ok(BaseEvidence {
        digest: hasher.finish(),
        review_diff,
    })
}

#[cfg(test)]
fn read_complete_evidence_file(
    path: &Path,
    root: &Path,
    description: &str,
    budget: &mut EvidenceBudget,
    control: Option<&RunControl>,
) -> Result<Vec<u8>, WorkerError> {
    let mut file = open_regular_contained(path, root, description)?;
    let mut contents = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        budget.check(control)?;
        let length = file.read(&mut buffer).map_err(|error| {
            WorkerError::new(
                "diff_failed",
                format!("Could not read complete {description}: {error}"),
            )
        })?;
        if length == 0 {
            break;
        }
        budget.consume(length, control)?;
        contents.extend_from_slice(&buffer[..length]);
    }
    Ok(contents)
}

fn stream_evidence_file(
    mut file: File,
    expected_length: u64,
    hasher: &mut StableHasher,
    content: &mut Vec<u8>,
    budget: &mut EvidenceBudget,
    control: Option<&RunControl>,
) -> Result<(), WorkerError> {
    let initial = file.metadata().map_err(|error| {
        WorkerError::new(
            "diff_failed",
            format!("Could not inspect review evidence: {error}"),
        )
    })?;
    if initial.len() != expected_length {
        return Err(evidence_state_changed());
    }
    let mut buffer = [0_u8; 16 * 1024];
    let mut streamed_length = 0_u64;
    loop {
        budget.check(control)?;
        let length = file.read(&mut buffer).map_err(|error| {
            WorkerError::new(
                "diff_failed",
                format!("Could not stream complete review evidence: {error}"),
            )
        })?;
        if length == 0 {
            break;
        }
        budget.consume(length, control)?;
        streamed_length = streamed_length
            .checked_add(u64::try_from(length).expect("buffer length fits in u64"))
            .ok_or_else(evidence_state_changed)?;
        hasher.update_stream(&buffer[..length]);
        content.extend_from_slice(&buffer[..length]);
    }
    let final_metadata = file.metadata().map_err(|error| {
        WorkerError::new(
            "diff_failed",
            format!("Could not recheck review evidence: {error}"),
        )
    })?;
    if streamed_length != expected_length
        || initial.len() != final_metadata.len()
        || initial.modified().ok() != final_metadata.modified().ok()
    {
        return Err(WorkerError::new(
            "diff_state_changed",
            "A reviewed file changed while Quorum was computing complete evidence. Retry after writes stop.",
        ));
    }
    Ok(())
}

fn append_review(destination: &mut Vec<u8>, bytes: &[u8]) -> Result<(), WorkerError> {
    if destination.len().saturating_add(bytes.len()) > MAX_REVIEW_DIFF_BYTES {
        return Err(review_evidence_too_large());
    }
    destination.extend_from_slice(bytes);
    Ok(())
}

fn review_evidence_too_large() -> WorkerError {
    WorkerError::new(
        "review_evidence_too_large",
        format!(
            "The complete base diff exceeds Quorum's {MAX_REVIEW_DIFF_BYTES} byte adversarial-review bound. Reduce or split the change before delivery; Quorum will not truncate evidence and continue."
        ),
    )
}

fn evidence_state_changed() -> WorkerError {
    WorkerError::new(
        "diff_state_changed",
        "The managed worktree changed while Quorum was capturing complete evidence. Retry after writes stop.",
    )
}

fn parse_nul_paths(
    bytes: &[u8],
    budget: &mut EvidenceBudget,
    control: Option<&RunControl>,
) -> Result<Vec<String>, WorkerError> {
    let mut pending = Vec::new();
    let mut paths = Vec::new();
    for byte in bytes {
        budget.check(control)?;
        if *byte == 0 {
            let relative = std::str::from_utf8(&pending).map_err(|error| {
                WorkerError::new(
                    "unsafe_review_file",
                    format!("An untracked path is not valid UTF-8: {error}"),
                )
            })?;
            if !relative.is_empty() {
                paths.push(relative.to_owned());
            }
            pending.clear();
        } else {
            pending.push(*byte);
            if pending.len() > 32 * 1024 {
                return Err(WorkerError::new(
                    "unsafe_review_file",
                    "An untracked path exceeds Quorum's safe path bound.",
                ));
            }
        }
    }
    if !pending.is_empty() {
        return Err(WorkerError::new(
            "diff_failed",
            "Git returned an unterminated untracked path list.",
        ));
    }
    Ok(paths)
}

fn review_file_mode(file: &File) -> Result<u32, WorkerError> {
    let metadata = file.metadata().map_err(|error| {
        WorkerError::new(
            "diff_failed",
            format!("Could not inspect an untracked review file mode: {error}"),
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Ok(if metadata.permissions().mode() & 0o111 == 0 {
            0o100_644
        } else {
            0o100_755
        })
    }
    #[cfg(not(unix))]
    {
        Ok(0o100_644)
    }
}

struct EvidenceBudget {
    started: Instant,
    bytes: usize,
}

impl EvidenceBudget {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            bytes: 0,
        }
    }

    fn check(&self, control: Option<&RunControl>) -> Result<(), WorkerError> {
        if control.is_some_and(RunControl::cancelled) {
            return Err(WorkerError::new(
                "cancelled",
                "Execution was cancelled while complete diff evidence was being captured.",
            ));
        }
        if self.started.elapsed() > EVIDENCE_TIMEOUT {
            return Err(WorkerError::new(
                "evidence_timeout",
                format!(
                    "Complete diff evidence exceeded Quorum's {} second time bound.",
                    EVIDENCE_TIMEOUT.as_secs()
                ),
            ));
        }
        Ok(())
    }

    fn consume(&mut self, length: usize, control: Option<&RunControl>) -> Result<(), WorkerError> {
        self.check(control)?;
        self.bytes = self.bytes.checked_add(length).ok_or_else(|| {
            WorkerError::new(
                "evidence_limit_exceeded",
                "Complete diff evidence exceeded Quorum's cumulative byte bound.",
            )
        })?;
        if self.bytes > MAX_EVIDENCE_BYTES {
            return Err(WorkerError::new(
                "evidence_limit_exceeded",
                format!(
                    "Complete diff evidence exceeded Quorum's {MAX_EVIDENCE_BYTES} byte cumulative capture-and-hashing bound."
                ),
            ));
        }
        Ok(())
    }
}

struct EvidenceArtifactCleanup {
    paths: [PathBuf; 2],
}

impl EvidenceArtifactCleanup {
    fn new(directory: &Path) -> Result<Self, WorkerError> {
        let cleanup = Self {
            paths: [
                directory.join("tracked.diff"),
                directory.join("untracked.paths"),
            ],
        };
        cleanup.remove()?;
        Ok(cleanup)
    }

    fn remove(&self) -> Result<(), WorkerError> {
        for path in &self.paths {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(WorkerError::new(
                        "diff_failed",
                        format!(
                            "Could not clean temporary evidence file {}: {error}",
                            path.display()
                        ),
                    ));
                }
            }
        }
        Ok(())
    }
}

impl Drop for EvidenceArtifactCleanup {
    fn drop(&mut self) {
        let _ = self.remove();
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

struct StableHasher {
    hasher: Sha256,
    record_open: bool,
}

impl StableHasher {
    fn new() -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"quorum-digest");
        hasher.update(1_u64.to_be_bytes());
        hasher.update(b"full-state");
        Self {
            hasher,
            record_open: false,
        }
    }

    fn begin_record(&mut self, kind: &[u8]) {
        debug_assert!(!self.record_open);
        self.hasher.update([1]);
        self.update_length_prefixed(kind);
        self.record_open = true;
    }

    fn field(&mut self, name: &[u8], value: &[u8]) {
        debug_assert!(self.record_open);
        self.begin_stream_field(
            name,
            u64::try_from(value.len()).expect("field length fits in u64"),
        );
        self.update_stream(value);
    }

    fn field_u32(&mut self, name: &[u8], value: u32) {
        self.field(name, &value.to_be_bytes());
    }

    fn field_u64(&mut self, name: &[u8], value: u64) {
        self.field(name, &value.to_be_bytes());
    }

    fn begin_stream_field(&mut self, name: &[u8], length: u64) {
        debug_assert!(self.record_open);
        self.hasher.update([2]);
        self.update_length_prefixed(name);
        self.hasher.update(length.to_be_bytes());
    }

    fn update_stream(&mut self, bytes: &[u8]) {
        debug_assert!(self.record_open);
        self.hasher.update(bytes);
    }

    fn end_record(&mut self) {
        debug_assert!(self.record_open);
        self.hasher.update([3]);
        self.record_open = false;
    }

    fn update_length_prefixed(&mut self, bytes: &[u8]) {
        self.hasher.update(
            u64::try_from(bytes.len())
                .expect("digest framing length fits in u64")
                .to_be_bytes(),
        );
        self.hasher.update(bytes);
    }

    fn finish(self) -> String {
        debug_assert!(!self.record_open);
        hex_encode(&self.hasher.finalize())
    }
}

fn initial_builder_prompt(plan: &str) -> String {
    format!(
        "Implement the persisted Quorum plan below completely in the current managed worktree. \
         You may read and write files and run shell commands only inside this worktree. Do not \
         push, open a pull request, rewrite existing history, reset, stash, or clean user work. \
         Preserve unrelated content. Finish with a concise summary of changes made.\n\n\
         PERSISTED_PLAN\n{plan}"
    )
}

fn verification_remediation_prompt(program: &str, arguments: &[String], evidence: &str) -> String {
    format!(
        "Verification failed in the managed worktree. Diagnose and fix the implementation, then \
         stop so Quorum can rerun verification. Do not reset, stash, clean, push, or open a pull \
         request.\n\nVERIFICATION_PROGRAM\n{program}\n\nVERIFICATION_ARGUMENTS_JSON\n{}\n\n\
         VERIFICATION_EVIDENCE\n{evidence}",
        serde_json::to_string(arguments).unwrap_or_else(|_| "[]".to_owned())
    )
}

fn findings_remediation_prompt(
    summary: &str,
    findings: &[ReviewFinding],
) -> Result<String, WorkerError> {
    let findings = serde_json::to_string_pretty(findings).map_err(|error| {
        WorkerError::new(
            "review_contract",
            format!("Could not serialize blocking findings: {error}"),
        )
    })?;
    Ok(format!(
        "An independent adversarial reviewer found the blocking issues below. Fix every blocking \
         issue in the managed worktree. Preserve unrelated work and do not reset, stash, clean, \
         push, or open a pull request. Stop after material fixes so Quorum can rerun verification \
         and request a focused re-review. Preserve each finding ID in your reasoning.\n\n\
         REVIEW_SUMMARY\n{summary}\n\nBLOCKING_FINDINGS_JSON\n{findings}"
    ))
}

fn reviewer_prompt(
    plan: &str,
    acceptance_intent: &str,
    base_commit: &str,
    base_diff: &str,
    verification: &str,
    iteration: usize,
) -> String {
    let focus = if iteration == 0 {
        "Perform an independent adversarial review of the implementation."
    } else {
        "Perform a focused independent re-review. Recheck prior concerns and all material changes."
    };
    format!(
        "{focus} Treat the persisted plan as intent, but report only concrete correctness, safety, \
         data-loss, security, or verification gaps visible in the supplied base diff and repository \
         context. Do not modify files or run shell commands. Use stable finding IDs and reuse an ID \
         for the same issue in later focused reviews. A blocking finding must prevent delivery; use \
         warning for non-blocking risk. Evaluate whether the implementation completely satisfies \
         both the persisted plan and the original acceptance intent.\n\nPERSISTED_PLAN\n{plan}\n\n\
         ACCEPTANCE_INTENT\n{acceptance_intent}\n\nBASE_COMMIT\n{base_commit}\n\nBASE_DIFF\n{base_diff}\n\n\
         VERIFICATION_EVIDENCE\n{verification}\n\nOUTPUT_CONTRACT\nReturn exactly one JSON object \
         with no Markdown fence or surrounding prose: \
         {{\"version\":{REVIEW_CONTRACT_VERSION},\"summary\":\"concise summary\",\
         \"findings\":[{{\"id\":\"stable-id\",\"severity\":\"blocking|warning\",\
         \"title\":\"title\",\"body\":\"actionable evidence\",\"path\":\"path or null\",\
         \"line\":1}}]}}. Use an empty findings array when no issue remains."
    )
}

fn process_failure(code: &str, prefix: &str, result: &ProcessResult) -> WorkerError {
    let evidence = process_evidence(result);
    WorkerError::new(code, format!("{prefix} {evidence}"))
}

fn copilot_session_confirmed(result: &ProcessResult) -> bool {
    result.success
        && String::from_utf8_lossy(&result.stdout).lines().any(|line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .is_some_and(|value| {
                    value.get("type").and_then(Value::as_str) == Some("assistant.message")
                })
        })
}

fn process_evidence(result: &ProcessResult) -> String {
    let stderr = String::from_utf8_lossy(&result.stderr);
    let stdout = String::from_utf8_lossy(&result.stdout);
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    if detail.is_empty() {
        format!("Process status: {}.", result.status)
    } else {
        let mut detail = detail.to_owned();
        if detail.len() > 16_000 {
            let mut boundary = 16_000;
            while !detail.is_char_boundary(boundary) {
                boundary -= 1;
            }
            detail.truncate(boundary);
            detail.push_str("\n[diagnostic truncated]");
        }
        format!("Process status: {}. Output:\n{detail}", result.status)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewEnvelope {
    version: u8,
    summary: String,
    findings: Vec<ReviewFinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewFinding {
    id: String,
    severity: String,
    title: String,
    body: String,
    path: Option<String>,
    line: Option<usize>,
}

fn parse_review_jsonl(output: &str) -> Result<ReviewEnvelope, WorkerError> {
    let mut envelopes = Vec::new();
    let mut malformed = None;
    for (index, line) in output.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let payload: Value = serde_json::from_str(line).map_err(|error| {
            WorkerError::new(
                "review_contract",
                format!(
                    "Copilot reviewer returned malformed JSONL on line {}: {error}",
                    index + 1
                ),
            )
        })?;
        collect_review_envelopes(&payload, &mut envelopes, &mut malformed);
    }
    if envelopes.is_empty() {
        return Err(WorkerError::new(
            "review_contract",
            malformed.map_or_else(
                || {
                    "Copilot reviewer completed without the required structured findings."
                        .to_owned()
                },
                |error| format!("Copilot reviewer returned malformed findings: {error}"),
            ),
        ));
    }
    let envelope = envelopes.pop().expect("checked non-empty");
    if envelopes.iter().any(|candidate| candidate != &envelope) {
        return Err(WorkerError::new(
            "review_contract",
            "Copilot reviewer returned conflicting structured findings.",
        ));
    }
    validate_review(envelope)
}

fn collect_review_envelopes(
    value: &Value,
    envelopes: &mut Vec<ReviewEnvelope>,
    malformed: &mut Option<String>,
) {
    match value {
        Value::String(content) => {
            let content = content.trim();
            if content.starts_with('{') {
                match serde_json::from_str::<Value>(content) {
                    Ok(nested) => collect_review_envelopes(&nested, envelopes, malformed),
                    Err(error)
                        if content.contains("\"version\"") || content.contains("\"findings\"") =>
                    {
                        *malformed = Some(error.to_string());
                    }
                    Err(_) => {}
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_review_envelopes(value, envelopes, malformed);
            }
        }
        Value::Object(values) => {
            if values.contains_key("version") || values.contains_key("findings") {
                match serde_json::from_value::<ReviewEnvelope>(value.clone()) {
                    Ok(envelope) => envelopes.push(envelope),
                    Err(error) => *malformed = Some(error.to_string()),
                }
            } else {
                for value in values.values() {
                    collect_review_envelopes(value, envelopes, malformed);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn validate_review(envelope: ReviewEnvelope) -> Result<ReviewEnvelope, WorkerError> {
    if envelope.version != REVIEW_CONTRACT_VERSION {
        return Err(WorkerError::new(
            "review_contract",
            format!(
                "Copilot reviewer returned contract version {}; expected {REVIEW_CONTRACT_VERSION}.",
                envelope.version
            ),
        ));
    }
    if envelope.summary.trim().is_empty() {
        return Err(WorkerError::new(
            "review_contract",
            "Copilot reviewer returned an empty summary.",
        ));
    }
    let mut ids = HashSet::new();
    for finding in &envelope.findings {
        if finding.id.trim().is_empty()
            || finding.title.trim().is_empty()
            || finding.body.trim().is_empty()
            || !matches!(finding.severity.as_str(), "blocking" | "warning")
            || !ids.insert(finding.id.as_str())
        {
            return Err(WorkerError::new(
                "review_contract",
                "Copilot reviewer returned an invalid or duplicate finding.",
            ));
        }
        if finding.path.as_deref().is_some_and(str::is_empty) || finding.line == Some(0) {
            return Err(WorkerError::new(
                "review_contract",
                "Copilot reviewer returned an invalid finding location.",
            ));
        }
    }
    Ok(envelope)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fs::{self, File, OpenOptions};
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use fs2::FileExt;
    use serde_json::json;
    use tempfile::tempdir;
    use uuid::Uuid;

    #[cfg(target_os = "macos")]
    use super::macos_sandbox_profile;
    use super::{
        collect_base_evidence, collect_base_evidence_bytes, copilot_environment,
        copilot_log_messages, copilot_session_confirmed, inherited_copilot_token, open_output_file,
        preflight_with_executables, process_evidence, resolve_executable, run_owned_process,
        validate_confinement_tree, CancelExecutionRequest, EvidenceBudget, ExecutionDetailDto,
        ExecutionProcessRunner, ExecutionService, ExecutionSupervisor, ProcessChunk,
        ProcessRequest, ProcessResult, ResolveExecutionFindingRequest, ResumeExecutionRequest,
        RunControl, StartExecutionRequest, SystemExecutionProcessRunner, MAX_EVIDENCE_BYTES,
        MAX_PROCESS_OUTPUT_BYTES, MAX_REVIEW_DIFF_BYTES,
    };
    use crate::state::AppStore;

    #[derive(Debug)]
    enum BuilderAction {
        Write { path: String, content: String },
        Fail,
        SpawnFail,
        Wait,
    }

    #[derive(Debug, Clone)]
    struct Call {
        program: String,
        arguments: Vec<String>,
    }

    #[derive(Default)]
    struct FakeRunner {
        builder_actions: Mutex<VecDeque<BuilderAction>>,
        reviews: Mutex<VecDeque<serde_json::Value>>,
        calls: Mutex<Vec<Call>>,
        builder_waiting: AtomicBool,
        truncate_reviews: bool,
        fail_next_review: AtomicBool,
        review_mutations: Mutex<VecDeque<(String, String)>>,
        verification_mutations: Mutex<VecDeque<(String, String)>>,
    }

    impl FakeRunner {
        fn new(
            builder_actions: impl IntoIterator<Item = BuilderAction>,
            reviews: impl IntoIterator<Item = serde_json::Value>,
        ) -> Self {
            Self {
                builder_actions: Mutex::new(builder_actions.into_iter().collect()),
                reviews: Mutex::new(reviews.into_iter().collect()),
                calls: Mutex::new(Vec::new()),
                builder_waiting: AtomicBool::new(false),
                truncate_reviews: false,
                fail_next_review: AtomicBool::new(false),
                review_mutations: Mutex::new(VecDeque::new()),
                verification_mutations: Mutex::new(VecDeque::new()),
            }
        }

        fn with_truncated_reviews(mut self) -> Self {
            self.truncate_reviews = true;
            self
        }

        fn with_failing_review(self) -> Self {
            self.fail_next_review.store(true, Ordering::SeqCst);
            self
        }

        fn with_review_mutation(self, path: &str, content: &str) -> Self {
            self.review_mutations
                .lock()
                .expect("review mutations lock")
                .push_back((path.to_owned(), content.to_owned()));
            self
        }

        fn with_verification_mutation(self, path: &str, content: &str) -> Self {
            self.verification_mutations
                .lock()
                .expect("verification mutations lock")
                .push_back((path.to_owned(), content.to_owned()));
            self
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.lock().expect("calls lock").clone()
        }
    }

    impl ExecutionProcessRunner for FakeRunner {
        #[allow(clippy::too_many_lines)]
        fn run(
            &self,
            request: &ProcessRequest,
            control: &RunControl,
            output: &mut dyn FnMut(ProcessChunk),
        ) -> std::io::Result<ProcessResult> {
            self.calls.lock().expect("calls lock").push(Call {
                program: request.program.clone(),
                arguments: request.arguments.clone(),
            });
            if Path::new(&request.program)
                .file_name()
                .and_then(|name| name.to_str())
                == Some("copilot")
            {
                let reviewer = request
                    .arguments
                    .iter()
                    .any(|argument| argument == "--deny-tool=write");
                if reviewer {
                    if self.fail_next_review.swap(false, Ordering::SeqCst) {
                        return Ok(ProcessResult {
                            success: false,
                            exit_code: Some(1),
                            status: "exit status: 1".to_owned(),
                            stdout: Vec::new(),
                            stderr: b"reviewer failed".to_vec(),
                            capture_truncated: false,
                        });
                    }
                    if let Some((path, content)) = self
                        .review_mutations
                        .lock()
                        .expect("review mutations lock")
                        .pop_front()
                    {
                        fs::write(request.cwd.join(path), content)?;
                    }
                    let review = self
                        .reviews
                        .lock()
                        .expect("reviews lock")
                        .pop_front()
                        .unwrap_or_else(
                            || json!({"version": 1, "summary": "No findings.", "findings": []}),
                        );
                    let event = json!({
                        "type": "assistant.message",
                        "content": review.to_string()
                    })
                    .to_string()
                        + "\n";
                    output(ProcessChunk {
                        stream: "stdout",
                        bytes: event.as_bytes().to_vec(),
                    });
                    let mut result = success_result(event.into_bytes());
                    result.capture_truncated = self.truncate_reviews;
                    return Ok(result);
                }
                let action = self
                    .builder_actions
                    .lock()
                    .expect("builder actions lock")
                    .pop_front()
                    .unwrap_or(BuilderAction::Fail);
                match action {
                    BuilderAction::Write { path, content } => {
                        fs::write(request.cwd.join(path), content)?;
                        let event =
                            b"{\"type\":\"assistant.message\",\"content\":\"implemented\"}\n";
                        output(ProcessChunk {
                            stream: "stdout",
                            bytes: event.to_vec(),
                        });
                        Ok(success_result(event.to_vec()))
                    }
                    BuilderAction::Fail => {
                        let message = b"builder failed";
                        output(ProcessChunk {
                            stream: "stderr",
                            bytes: message.to_vec(),
                        });
                        Ok(ProcessResult {
                            success: false,
                            exit_code: Some(1),
                            status: "exit status: 1".to_owned(),
                            stdout: Vec::new(),
                            stderr: message.to_vec(),
                            capture_truncated: false,
                        })
                    }
                    BuilderAction::SpawnFail => {
                        Err(std::io::Error::other("simulated spawn failure"))
                    }
                    BuilderAction::Wait => {
                        self.builder_waiting.store(true, Ordering::SeqCst);
                        while !control.cancelled() {
                            thread::sleep(Duration::from_millis(10));
                        }
                        Ok(ProcessResult {
                            success: false,
                            exit_code: None,
                            status: "cancelled".to_owned(),
                            stdout: Vec::new(),
                            stderr: Vec::new(),
                            capture_truncated: false,
                        })
                    }
                }
            } else {
                let mut command = Command::new(&request.program);
                if request.clear_git_environment {
                    super::clear_git_environment(&mut command);
                }
                let result = command
                    .args(&request.arguments)
                    .envs(
                        request
                            .environment
                            .iter()
                            .map(|(name, value)| (name.as_str(), value.as_str())),
                    )
                    .current_dir(&request.cwd)
                    .output()?;
                let is_verification = Path::new(&request.program)
                    .file_name()
                    .is_some_and(|name| name == "make")
                    || request.arguments.iter().any(|argument| {
                        Path::new(argument)
                            .file_name()
                            .is_some_and(|name| name == "make")
                    });
                if is_verification {
                    if let Some((path, content)) = self
                        .verification_mutations
                        .lock()
                        .expect("verification mutations lock")
                        .pop_front()
                    {
                        fs::write(request.cwd.join(path), content)?;
                    }
                }
                let stdout = if let Some(path) = &request.stdout_path {
                    open_output_file(path)?.write_all(&result.stdout)?;
                    Vec::new()
                } else {
                    result.stdout.clone()
                };
                if !stdout.is_empty() {
                    output(ProcessChunk {
                        stream: "stdout",
                        bytes: stdout.clone(),
                    });
                }
                if !result.stderr.is_empty() {
                    output(ProcessChunk {
                        stream: "stderr",
                        bytes: result.stderr.clone(),
                    });
                }
                Ok(ProcessResult {
                    success: result.status.success(),
                    exit_code: result.status.code(),
                    status: result.status.to_string(),
                    stdout,
                    stderr: result.stderr,
                    capture_truncated: false,
                })
            }
        }
    }

    struct PanicRunner;

    impl ExecutionProcessRunner for PanicRunner {
        fn run(
            &self,
            _request: &ProcessRequest,
            _control: &RunControl,
            _output: &mut dyn FnMut(ProcessChunk),
        ) -> std::io::Result<ProcessResult> {
            panic!("simulated runner panic");
        }
    }

    fn success_result(stdout: Vec<u8>) -> ProcessResult {
        ProcessResult {
            success: true,
            exit_code: Some(0),
            status: "exit status: 0".to_owned(),
            stdout,
            stderr: Vec::new(),
            capture_truncated: false,
        }
    }

    struct Harness {
        _directory: tempfile::TempDir,
        repository: PathBuf,
        store: Arc<AppStore>,
        queue_entry_id: String,
    }

    impl Harness {
        fn new(with_verification: bool) -> Self {
            let directory = tempdir().expect("temp directory");
            let repository = directory.path().join("repository");
            let app_data = directory.path().join("app-data");
            fs::create_dir_all(&repository).expect("repository directory");
            run_git(&repository, &["init", "-b", "main"]);
            run_git(&repository, &["config", "user.email", "quorum@example.com"]);
            run_git(&repository, &["config", "user.name", "Quorum Tests"]);
            fs::write(repository.join("README.md"), "# Fixture\n").expect("fixture readme");
            if with_verification {
                fs::write(
                    repository.join("Makefile"),
                    "check:\n\t@test -f implemented.txt\n",
                )
                .expect("fixture Makefile");
            }
            run_git(&repository, &["add", "."]);
            run_git(&repository, &["commit", "-m", "base"]);
            let store = Arc::new(AppStore::open(&app_data).expect("store"));
            let queue_entry_id = "queue".to_owned();
            store
                .with_connection(|connection| {
                    connection.execute(
                        "INSERT INTO repositories (
                           id, root_path, display_name, created_at, updated_at
                         ) VALUES ('repository', ?1, 'repository', 'now', 'now')",
                        [repository.to_string_lossy().as_ref()],
                    )?;
                    connection.execute_batch(
                        "INSERT INTO work_items (
                           id, repository_id, title, source_kind, source_metadata_json,
                           markdown_body, lifecycle_status, require_plan_approval,
                           created_at, updated_at
                         ) VALUES (
                           'work', 'repository', 'Implement fixture', 'inline_markdown',
                           '{\"kind\":\"inline_markdown\"}', '# Requirements', 'open', 0,
                           'now', 'now'
                         );
                         INSERT INTO planning_runs (
                           id, work_item_id, status, created_at, updated_at, completed_at
                         ) VALUES (
                           'planning', 'work', 'succeeded', 'now', 'now', 'now'
                         );
                         INSERT INTO plans (
                           id, work_item_id, revision, markdown_body, approval_policy,
                           approval_status, created_at, updated_at, planning_run_id,
                           queue_eligibility_key, queue_eligible_at
                         ) VALUES (
                           'plan', 'work', 1, '# Plan\n\nCreate implemented.txt.',
                           'not_required', 'draft', 'now', 'now', 'planning',
                           'eligible', 'now'
                         );
                         INSERT INTO queue_entries (
                           id, work_item_id, position, scheduling_status, created_at,
                           updated_at, plan_id, idempotency_key
                         ) VALUES (
                           'queue', 'work', 0, 'queued', 'now', 'now', 'plan', 'eligible'
                         );",
                    )?;
                    Ok(())
                })
                .expect("seed");
            Self {
                _directory: directory,
                repository,
                store,
                queue_entry_id,
            }
        }

        fn service(&self, runner: Arc<dyn ExecutionProcessRunner>) -> ExecutionService {
            ExecutionService::with_runner(
                Arc::clone(&self.store),
                Arc::new(ExecutionSupervisor::default()),
                runner,
            )
        }
    }

    fn run_git(repository: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(repository)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            arguments,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn start(service: &ExecutionService, queue_entry_id: &str) -> ExecutionDetailDto {
        service
            .start(&StartExecutionRequest {
                queue_entry_id: queue_entry_id.to_owned(),
                idempotency_key: Uuid::new_v4().to_string(),
            })
            .expect("start execution")
    }

    fn wait_for_status(
        service: &ExecutionService,
        run_id: &str,
        statuses: &[&str],
    ) -> ExecutionDetailDto {
        for _ in 0..400 {
            let detail = service.detail(run_id).expect("execution detail");
            if statuses.contains(&detail.run.status.as_str()) {
                return detail;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("execution did not reach one of {statuses:?}");
    }

    fn blocking_review() -> serde_json::Value {
        json!({
            "version": 1,
            "summary": "A blocking defect remains.",
            "findings": [{
                "id": "missing-validation",
                "severity": "blocking",
                "title": "Missing validation",
                "body": "The implementation accepts invalid input.",
                "path": "implemented.txt",
                "line": 1
            }]
        })
    }

    #[test]
    fn executes_plan_in_isolated_worktree_and_reaches_delivery() {
        let harness = Harness::new(true);
        let runner = Arc::new(FakeRunner::new(
            [BuilderAction::Write {
                path: "implemented.txt".to_owned(),
                content: "implemented\n".to_owned(),
            }],
            [json!({"version": 1, "summary": "Looks correct.", "findings": []})],
        ));
        let service = harness.service(runner.clone());
        let started = start(&service, &harness.queue_entry_id);
        let completed = wait_for_status(&service, &started.run.id, &["ready"]);

        assert!(completed.delivery_ready);
        assert_eq!(completed.run.phase, "delivery");
        assert_eq!(completed.run.outcome, "succeeded");
        assert!(Path::new(&completed.run.worktree_path)
            .join("implemented.txt")
            .is_file());
        assert!(!harness.repository.join("implemented.txt").exists());
        assert!(completed
            .run
            .branch_name
            .starts_with("quorum/implement-fixture-"));
        let source_status = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&harness.repository)
            .output()
            .expect("source status");
        assert!(source_status.stdout.is_empty());

        let copilot_calls = runner
            .calls()
            .into_iter()
            .filter(|call| {
                Path::new(&call.program)
                    .file_name()
                    .and_then(|name| name.to_str())
                    == Some("copilot")
            })
            .collect::<Vec<_>>();
        assert_eq!(copilot_calls.len(), 2);
        assert!(copilot_calls[0]
            .arguments
            .windows(2)
            .any(|pair| pair == ["--model", "gpt-5.6-sol"]));
        assert!(copilot_calls.iter().all(|call| !call
            .arguments
            .iter()
            .any(|argument| argument == "--allow-all-paths")));
        assert!(copilot_calls.iter().all(|call| call
            .arguments
            .iter()
            .any(|argument| argument == "--allow-all-tools")));
        assert!(copilot_calls[1]
            .arguments
            .windows(2)
            .any(|pair| pair == ["--model", "claude-opus-5"]));
        assert_ne!(
            completed.run.builder_session_name,
            completed.run.reviewer_session_name
        );
    }

    #[cfg(unix)]
    #[test]
    fn worktree_provisioning_disables_repository_checkout_hooks() {
        use std::os::unix::fs::PermissionsExt;

        let harness = Harness::new(true);
        let marker = harness
            .repository
            .parent()
            .expect("repository parent")
            .join("post-checkout-ran");
        let hook = harness.repository.join(".git/hooks/post-checkout");
        fs::write(
            &hook,
            format!("#!/bin/sh\n/usr/bin/touch '{}'\n", marker.display()),
        )
        .expect("post-checkout hook");
        let mut permissions = fs::metadata(&hook).expect("hook metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&hook, permissions).expect("executable hook");

        let service = harness.service(Arc::new(FakeRunner::new([BuilderAction::Fail], [])));
        let started = start(&service, &harness.queue_entry_id);
        let blocked = wait_for_status(&service, &started.run.id, &["blocked"]);

        assert_eq!(blocked.run.error_code.as_deref(), Some("builder_failed"));
        assert!(Path::new(&blocked.run.worktree_path).is_dir());
        assert!(!marker.exists(), "repository post-checkout hook executed");
    }

    #[test]
    fn worktree_provisioning_blocks_executable_checkout_filters() {
        let harness = Harness::new(true);
        let marker = harness
            .repository
            .parent()
            .expect("repository parent")
            .join("checkout-filter-ran");
        let filter = format!("/usr/bin/touch '{}'", marker.display());
        run_git(
            &harness.repository,
            &["config", "filter.quorum-unsafe.smudge", &filter],
        );
        fs::write(
            harness.repository.join(".gitattributes"),
            "filtered.txt filter=quorum-unsafe\n",
        )
        .expect("attributes");
        fs::write(harness.repository.join("filtered.txt"), "source\n").expect("filtered file");
        run_git(
            &harness.repository,
            &["add", ".gitattributes", "filtered.txt"],
        );
        run_git(
            &harness.repository,
            &["commit", "-m", "add filtered fixture"],
        );

        let service = harness.service(Arc::new(FakeRunner::new([BuilderAction::Fail], [])));
        let started = start(&service, &harness.queue_entry_id);
        let blocked = wait_for_status(&service, &started.run.id, &["blocked"]);

        assert_eq!(
            blocked.run.error_code.as_deref(),
            Some("unsafe_checkout_filter")
        );
        assert!(blocked
            .run
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("filter.quorum-unsafe.smudge")));
        assert!(!Path::new(&blocked.run.worktree_path).exists());
        assert!(!marker.exists(), "repository checkout filter executed");
    }

    #[test]
    fn worktree_provisioning_blocks_untrusted_filter_attributes() {
        let harness = Harness::new(true);
        fs::write(
            harness.repository.join(".gitattributes"),
            "filtered.txt filter=external-driver\n",
        )
        .expect("attributes");
        fs::write(harness.repository.join("filtered.txt"), "source\n").expect("filtered file");
        run_git(
            &harness.repository,
            &["add", ".gitattributes", "filtered.txt"],
        );
        run_git(
            &harness.repository,
            &["commit", "-m", "add filtered fixture"],
        );

        let service = harness.service(Arc::new(FakeRunner::new([BuilderAction::Fail], [])));
        let started = start(&service, &harness.queue_entry_id);
        let blocked = wait_for_status(&service, &started.run.id, &["blocked"]);

        assert_eq!(
            blocked.run.error_code.as_deref(),
            Some("unsafe_checkout_filter")
        );
        assert!(blocked
            .run
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("filtered.txt")));
        assert!(!Path::new(&blocked.run.worktree_path).exists());
    }

    #[test]
    fn remediates_blocking_findings_then_runs_verification_and_focused_review() {
        let harness = Harness::new(true);
        let runner = Arc::new(FakeRunner::new(
            [
                BuilderAction::Write {
                    path: "implemented.txt".to_owned(),
                    content: "bad\n".to_owned(),
                },
                BuilderAction::Write {
                    path: "implemented.txt".to_owned(),
                    content: "fixed\n".to_owned(),
                },
            ],
            [
                blocking_review(),
                json!({"version": 1, "summary": "The blocking defect is fixed.", "findings": []}),
            ],
        ));
        let service = harness.service(runner.clone());
        let started = start(&service, &harness.queue_entry_id);
        let completed = wait_for_status(&service, &started.run.id, &["ready"]);

        assert_eq!(completed.run.iteration, 1);
        assert_eq!(completed.findings.len(), 1);
        assert_eq!(completed.findings[0].status, "fixed");
        let verification_count: usize = harness
            .store
            .with_connection(|connection| {
                connection
                    .query_row(
                        "SELECT count(*) FROM execution_commands
                         WHERE run_id = ?1 AND phase = 'verifying'",
                        [&started.run.id],
                        |row| row.get(0),
                    )
                    .map_err(Into::into)
            })
            .expect("verification count");
        assert_eq!(verification_count, 2);
        let reviewer_prompts = runner
            .calls()
            .into_iter()
            .filter(|call| {
                call.arguments
                    .iter()
                    .any(|argument| argument == "--deny-tool=write")
            })
            .filter_map(|call| {
                call.arguments
                    .windows(2)
                    .find(|pair| pair[0] == "-p")
                    .map(|pair| pair[1].clone())
            })
            .collect::<Vec<_>>();
        assert_eq!(reviewer_prompts.len(), 2);
        assert!(reviewer_prompts.iter().all(|prompt| {
            prompt.contains("PERSISTED_PLAN\n# Plan")
                && prompt.contains("ACCEPTANCE_INTENT\n# Requirements")
        }));
    }

    #[test]
    fn cancellation_targets_owned_worker_and_preserves_state() {
        let harness = Harness::new(true);
        let runner = Arc::new(FakeRunner::new([BuilderAction::Wait], []));
        let supervisor = Arc::new(ExecutionSupervisor::default());
        let service = ExecutionService::with_runner(
            Arc::clone(&harness.store),
            Arc::clone(&supervisor),
            runner.clone(),
        );
        let started = start(&service, &harness.queue_entry_id);
        for _ in 0..200 {
            if runner.builder_waiting.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(runner.builder_waiting.load(Ordering::SeqCst));

        service
            .cancel(&CancelExecutionRequest {
                run_id: started.run.id.clone(),
            })
            .expect("cancel execution");
        let cancelled = wait_for_status(&service, &started.run.id, &["cancelled"]);
        assert_eq!(cancelled.run.outcome, "cancelled");
        assert!(!cancelled.attempts.is_empty());
        assert_eq!(
            cancelled.attempts.last().expect("attempt").status,
            "cancelled"
        );
        assert!(!harness.repository.join("implemented.txt").exists());
    }

    #[test]
    fn dirty_source_and_missing_verification_block_without_mutating_user_work() {
        let dirty = Harness::new(true);
        fs::write(dirty.repository.join("user-change.txt"), "keep me\n").expect("dirty file");
        let service = dirty.service(Arc::new(FakeRunner::default()));
        let blocked = start(&service, &dirty.queue_entry_id);
        assert_eq!(blocked.run.status, "blocked");
        assert_eq!(blocked.run.error_code.as_deref(), Some("dirty_source"));
        assert_eq!(
            fs::read_to_string(dirty.repository.join("user-change.txt")).expect("dirty content"),
            "keep me\n"
        );
        let branch = Command::new("git")
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{}", blocked.run.branch_name),
            ])
            .current_dir(&dirty.repository)
            .status()
            .expect("branch probe");
        assert!(!branch.success());

        fs::remove_file(dirty.repository.join("user-change.txt")).expect("clean source");
        let resume_runner = Arc::new(FakeRunner::new(
            [BuilderAction::Write {
                path: "implemented.txt".to_owned(),
                content: "resumed after clean preflight\n".to_owned(),
            }],
            [json!({"version": 1, "summary": "No findings.", "findings": []})],
        ));
        let resume_service = dirty.service(resume_runner);
        resume_service
            .resume(&ResumeExecutionRequest {
                run_id: blocked.run.id.clone(),
            })
            .expect("resume after cleaning source");
        let ready = wait_for_status(&resume_service, &blocked.run.id, &["ready"]);
        assert!(ready.delivery_ready);

        let no_verification = Harness::new(false);
        let service = no_verification.service(Arc::new(FakeRunner::default()));
        let blocked = start(&service, &no_verification.queue_entry_id);
        assert_eq!(blocked.run.status, "blocked");
        assert_eq!(
            blocked.run.error_code.as_deref(),
            Some("verification_unavailable")
        );
        assert!(blocked
            .run
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("Makefile `check`")));
    }

    #[test]
    fn preflight_fails_when_copilot_executable_is_unavailable() {
        let harness = Harness::new(true);
        let git = resolve_executable("git");
        let result = preflight_with_executables(
            harness.repository.to_string_lossy().as_ref(),
            "quorum/missing-copilot",
            &harness
                .store
                .app_data_dir()
                .join("worktrees/missing-copilot"),
            git.as_deref(),
            None,
        );
        let error = result.error.expect("missing Copilot must block preflight");
        assert_eq!(error.code, "missing_copilot");
        assert!(error.message.contains("was not found on PATH"));
        assert!(result.copilot_program.is_none());
    }

    #[test]
    fn nonzero_builder_launch_remains_fresh_in_a_new_attempt() {
        let harness = Harness::new(true);
        let first_runner = Arc::new(FakeRunner::new([BuilderAction::Fail], []));
        let supervisor = Arc::new(ExecutionSupervisor::default());
        let first_service = ExecutionService::with_runner(
            Arc::clone(&harness.store),
            Arc::clone(&supervisor),
            first_runner,
        );
        let started = start(&first_service, &harness.queue_entry_id);
        let blocked = wait_for_status(&first_service, &started.run.id, &["blocked"]);
        assert_eq!(blocked.run.error_code.as_deref(), Some("builder_failed"));

        let second_runner = Arc::new(FakeRunner::new(
            [BuilderAction::Write {
                path: "implemented.txt".to_owned(),
                content: "resumed\n".to_owned(),
            }],
            [json!({"version": 1, "summary": "No findings.", "findings": []})],
        ));
        let second_service = ExecutionService::with_runner(
            Arc::clone(&harness.store),
            supervisor,
            second_runner.clone(),
        );
        fs::write(harness.repository.join("user-change.txt"), "preserve\n")
            .expect("dirty source before resume");
        let unsafe_resume = second_service
            .resume(&ResumeExecutionRequest {
                run_id: started.run.id.clone(),
            })
            .expect("persist unsafe resume diagnostic");
        assert_eq!(
            unsafe_resume.run.error_code.as_deref(),
            Some("dirty_source")
        );
        assert_eq!(unsafe_resume.attempts.len(), 1);
        assert_eq!(
            fs::read_to_string(harness.repository.join("user-change.txt"))
                .expect("preserved source change"),
            "preserve\n"
        );
        fs::remove_file(harness.repository.join("user-change.txt")).expect("clean source");
        second_service
            .resume(&ResumeExecutionRequest {
                run_id: started.run.id.clone(),
            })
            .expect("resume execution after cleaning source");
        let completed = wait_for_status(&second_service, &started.run.id, &["ready"]);
        assert_eq!(completed.attempts.len(), 2);
        let builder_call = second_runner
            .calls()
            .into_iter()
            .find(|call| {
                Path::new(&call.program)
                    .file_name()
                    .and_then(|name| name.to_str())
                    == Some("copilot")
                    && !call
                        .arguments
                        .iter()
                        .any(|argument| argument == "--deny-tool=write")
            })
            .expect("resumed builder call");
        assert!(builder_call
            .arguments
            .iter()
            .any(|argument| argument == "--session-id"));
        assert!(!builder_call
            .arguments
            .iter()
            .any(|argument| argument.starts_with("--resume=")));
    }

    #[test]
    fn bounds_review_loop_and_allows_explicit_blocking_disposition() {
        let harness = Harness::new(true);
        let runner = Arc::new(FakeRunner::new(
            [
                BuilderAction::Write {
                    path: "implemented.txt".to_owned(),
                    content: "iteration 0\n".to_owned(),
                },
                BuilderAction::Write {
                    path: "implemented.txt".to_owned(),
                    content: "iteration 1\n".to_owned(),
                },
                BuilderAction::Write {
                    path: "implemented.txt".to_owned(),
                    content: "iteration 2\n".to_owned(),
                },
                BuilderAction::Write {
                    path: "implemented.txt".to_owned(),
                    content: "iteration 3\n".to_owned(),
                },
            ],
            [
                blocking_review(),
                blocking_review(),
                blocking_review(),
                blocking_review(),
            ],
        ));
        let service = harness.service(runner);
        let started = start(&service, &harness.queue_entry_id);
        let blocked = wait_for_status(&service, &started.run.id, &["blocked"]);
        assert_eq!(blocked.run.iteration, 3);
        assert_eq!(blocked.run.error_code.as_deref(), Some("blocking_findings"));
        assert_eq!(blocked.blocking_finding_count, 1);

        let ready = service
            .resolve_finding(&ResolveExecutionFindingRequest {
                run_id: started.run.id,
                finding_id: blocked.findings[0].id.clone(),
                disposition_note:
                    "Accepted for this delivery because upstream validation is authoritative."
                        .to_owned(),
            })
            .expect("resolve finding");
        assert!(ready.delivery_ready);
        assert_eq!(ready.findings[0].status, "resolved");
        assert_eq!(ready.run.outcome, "succeeded");
    }

    #[test]
    fn content_changed_during_review_never_reaches_delivery() {
        let harness = Harness::new(true);
        let runner = Arc::new(
            FakeRunner::new(
                [BuilderAction::Write {
                    path: "implemented.txt".to_owned(),
                    content: "verified\n".to_owned(),
                }],
                [json!({"version": 1, "summary": "No findings.", "findings": []})],
            )
            .with_review_mutation("implemented.txt", "changed during review\n"),
        );
        let service = harness.service(runner);
        let started = start(&service, &harness.queue_entry_id);
        let blocked = wait_for_status(&service, &started.run.id, &["blocked"]);
        assert_eq!(
            blocked.run.error_code.as_deref(),
            Some("state_changed_after_review")
        );
        assert_eq!(blocked.run.current_step, "verifying");
        assert!(!blocked.delivery_ready);
        let digests: (Option<String>, Option<String>) = harness
            .store
            .with_connection(|connection| {
                connection
                    .query_row(
                        "SELECT verified_state_digest, reviewed_state_digest
                         FROM execution_runs WHERE run_id = ?1",
                        [&started.run.id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(Into::into)
            })
            .expect("delivery digests");
        assert_eq!(digests, (None, None));
    }

    #[test]
    fn content_changed_during_verification_cannot_be_certified() {
        let harness = Harness::new(true);
        let runner = Arc::new(
            FakeRunner::new(
                [BuilderAction::Write {
                    path: "implemented.txt".to_owned(),
                    content: "verified input\n".to_owned(),
                }],
                [json!({"version": 1, "summary": "No findings.", "findings": []})],
            )
            .with_verification_mutation("implemented.txt", "changed during verification\n"),
        );
        let service = harness.service(runner.clone());
        let started = start(&service, &harness.queue_entry_id);
        let blocked = wait_for_status(&service, &started.run.id, &["blocked"]);
        assert_eq!(
            blocked.run.error_code.as_deref(),
            Some("state_changed_during_verification")
        );
        assert_eq!(blocked.run.current_step, "verifying");
        assert!(!blocked.delivery_ready);
        assert!(!runner.calls().iter().any(|call| {
            call.arguments
                .iter()
                .any(|argument| argument == "--deny-tool=write")
        }));
        let verified: Option<String> = harness
            .store
            .with_connection(|connection| {
                connection
                    .query_row(
                        "SELECT verified_state_digest FROM execution_runs WHERE run_id = ?1",
                        [&started.run.id],
                        |row| row.get(0),
                    )
                    .map_err(Into::into)
            })
            .expect("verified digest");
        assert!(verified.is_none());
    }

    #[test]
    fn explicit_disposition_rechecks_reviewed_content_before_ready() {
        let harness = Harness::new(true);
        let runner = Arc::new(FakeRunner::new(
            [
                BuilderAction::Write {
                    path: "implemented.txt".to_owned(),
                    content: "iteration 0\n".to_owned(),
                },
                BuilderAction::Write {
                    path: "implemented.txt".to_owned(),
                    content: "iteration 1\n".to_owned(),
                },
                BuilderAction::Write {
                    path: "implemented.txt".to_owned(),
                    content: "iteration 2\n".to_owned(),
                },
                BuilderAction::Write {
                    path: "implemented.txt".to_owned(),
                    content: "iteration 3\n".to_owned(),
                },
            ],
            [
                blocking_review(),
                blocking_review(),
                blocking_review(),
                blocking_review(),
            ],
        ));
        let service = harness.service(runner);
        let started = start(&service, &harness.queue_entry_id);
        let blocked = wait_for_status(&service, &started.run.id, &["blocked"]);
        fs::write(
            Path::new(&blocked.run.worktree_path).join("implemented.txt"),
            "changed after review\n",
        )
        .expect("mutate reviewed content");

        let dispositioned = service
            .resolve_finding(&ResolveExecutionFindingRequest {
                run_id: started.run.id,
                finding_id: blocked.findings[0].id.clone(),
                disposition_note: "Accepted independently of this mutation.".to_owned(),
            })
            .expect("save disposition");
        assert_eq!(
            dispositioned.run.error_code.as_deref(),
            Some("state_changed_after_review")
        );
        assert_eq!(dispositioned.run.current_step, "verifying");
        assert!(!dispositioned.delivery_ready);
        assert_eq!(dispositioned.findings[0].status, "resolved");
    }

    #[test]
    fn resume_rechecks_unclaimed_conflicts_and_owned_worktree_identity() {
        let dirty = Harness::new(true);
        fs::write(dirty.repository.join("user-change.txt"), "preserve\n").expect("dirty source");
        let service = dirty.service(Arc::new(FakeRunner::default()));
        let blocked = start(&service, &dirty.queue_entry_id);
        run_git(
            &dirty.repository,
            &["branch", blocked.run.branch_name.as_str()],
        );
        fs::remove_file(dirty.repository.join("user-change.txt")).expect("clean source");
        let still_blocked = service
            .resume(&ResumeExecutionRequest {
                run_id: blocked.run.id,
            })
            .expect("persist conflict");
        assert_eq!(
            still_blocked.run.error_code.as_deref(),
            Some("branch_conflict")
        );
        assert_eq!(still_blocked.attempts.len(), 1);

        let owned = Harness::new(true);
        let first = owned.service(Arc::new(FakeRunner::new([BuilderAction::Fail], [])));
        let started = start(&first, &owned.queue_entry_id);
        let failed = wait_for_status(&first, &started.run.id, &["blocked"]);
        run_git(
            Path::new(&failed.run.worktree_path),
            &["switch", "-c", "intruder-branch"],
        );
        let resume = owned.service(Arc::new(FakeRunner::default()));
        let conflict = resume
            .resume(&ResumeExecutionRequest {
                run_id: failed.run.id,
            })
            .expect("persist ownership conflict");
        assert_eq!(
            conflict.run.error_code.as_deref(),
            Some("worktree_branch_conflict")
        );
        assert_eq!(conflict.attempts.len(), 1);
    }

    #[test]
    fn resume_reconciles_durable_ownership_stages_without_ambiguous_adoption() {
        let missing_claim = Harness::new(true);
        let first = missing_claim.service(Arc::new(FakeRunner::new([BuilderAction::Fail], [])));
        let started = start(&first, &missing_claim.queue_entry_id);
        let blocked = wait_for_status(&first, &started.run.id, &["blocked"]);
        let claim_path = first
            .ownership_claim_path(&started.run.id)
            .expect("claim path");
        fs::remove_file(&claim_path).expect("simulate crash before claim persistence");
        let retry = missing_claim.service(Arc::new(FakeRunner::new(
            [BuilderAction::Write {
                path: "implemented.txt".to_owned(),
                content: "resumed after claim recovery\n".to_owned(),
            }],
            [json!({"version": 1, "summary": "No findings.", "findings": []})],
        )));
        retry
            .resume(&ResumeExecutionRequest {
                run_id: blocked.run.id.clone(),
            })
            .expect("resume missing claim");
        let ready = wait_for_status(&retry, &blocked.run.id, &["ready"]);
        assert!(ready.delivery_ready);
        assert!(claim_path.is_file());

        let missing_metadata = Harness::new(true);
        let first = missing_metadata.service(Arc::new(FakeRunner::new([BuilderAction::Fail], [])));
        let started = start(&first, &missing_metadata.queue_entry_id);
        let blocked = wait_for_status(&first, &started.run.id, &["blocked"]);
        missing_metadata
            .store
            .with_connection(|connection| {
                connection.execute(
                    "UPDATE execution_runs
                     SET git_metadata_json = NULL, ownership_verified_at = NULL
                     WHERE run_id = ?1",
                    [&blocked.run.id],
                )?;
                Ok(())
            })
            .expect("simulate crash before Git metadata persistence");
        let retry = missing_metadata.service(Arc::new(FakeRunner::new(
            [BuilderAction::Write {
                path: "implemented.txt".to_owned(),
                content: "resumed after metadata recovery\n".to_owned(),
            }],
            [json!({"version": 1, "summary": "No findings.", "findings": []})],
        )));
        retry
            .resume(&ResumeExecutionRequest {
                run_id: blocked.run.id.clone(),
            })
            .expect("resume missing metadata");
        let ready = wait_for_status(&retry, &blocked.run.id, &["ready"]);
        assert!(ready.delivery_ready);
        let persisted: Option<String> = missing_metadata
            .store
            .with_connection(|connection| {
                connection
                    .query_row(
                        "SELECT git_metadata_json FROM execution_runs WHERE run_id = ?1",
                        [&blocked.run.id],
                        |row| row.get(0),
                    )
                    .map_err(Into::into)
            })
            .expect("persisted Git metadata");
        assert!(persisted.is_some());

        let ambiguous = Harness::new(true);
        let first = ambiguous.service(Arc::new(FakeRunner::new([BuilderAction::Fail], [])));
        let started = start(&first, &ambiguous.queue_entry_id);
        let blocked = wait_for_status(&first, &started.run.id, &["blocked"]);
        ambiguous
            .store
            .with_connection(|connection| {
                connection.execute(
                    "UPDATE execution_runs
                     SET git_metadata_json = NULL, ownership_verified_at = NULL
                     WHERE run_id = ?1",
                    [&blocked.run.id],
                )?;
                Ok(())
            })
            .expect("clear Git metadata");
        fs::write(
            Path::new(&blocked.run.worktree_path).join("unexpected.txt"),
            "ambiguous\n",
        )
        .expect("introduce ambiguous content");
        let retry = ambiguous.service(Arc::new(FakeRunner::default()));
        let conflict = retry
            .resume(&ResumeExecutionRequest {
                run_id: blocked.run.id,
            })
            .expect("persist ambiguous adoption conflict");
        assert_eq!(
            conflict.run.error_code.as_deref(),
            Some("worktree_ownership_conflict")
        );
        assert_eq!(conflict.attempts.len(), 1);
    }

    #[test]
    fn forged_worktree_git_pointer_is_detected_after_untrusted_command() {
        let harness = Harness::new(true);
        let runner = Arc::new(FakeRunner::new(
            [BuilderAction::Write {
                path: ".git".to_owned(),
                content: "gitdir: /nonexistent/forged\n".to_owned(),
            }],
            [],
        ));
        let service = harness.service(runner.clone());
        let started = start(&service, &harness.queue_entry_id);
        let blocked = wait_for_status(&service, &started.run.id, &["blocked"]);
        assert_eq!(
            blocked.run.error_code.as_deref(),
            Some("git_metadata_changed")
        );
        assert!(!blocked.delivery_ready);
        assert!(!runner.calls().iter().any(|call| {
            call.arguments
                .iter()
                .any(|argument| argument == "--deny-tool=write")
        }));
    }

    #[test]
    fn spawn_failure_keeps_builder_session_fresh_for_retry() {
        let harness = Harness::new(true);
        let first = harness.service(Arc::new(FakeRunner::new([BuilderAction::SpawnFail], [])));
        let started = start(&first, &harness.queue_entry_id);
        let blocked = wait_for_status(&first, &started.run.id, &["blocked"]);
        assert_eq!(
            blocked.run.error_code.as_deref(),
            Some("process_start_failed")
        );
        let (state, first_session_id): (String, String) = harness
            .store
            .with_connection(|connection| {
                connection
                    .query_row(
                        "SELECT builder_session_state, builder_session_id
                         FROM execution_runs WHERE run_id = ?1",
                        [&started.run.id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(Into::into)
            })
            .expect("session state");
        assert_eq!(state, "not_started");

        let runner = Arc::new(FakeRunner::new(
            [BuilderAction::Write {
                path: "implemented.txt".to_owned(),
                content: "fresh retry\n".to_owned(),
            }],
            [json!({"version": 1, "summary": "No findings.", "findings": []})],
        ));
        let retry = harness.service(runner.clone());
        retry
            .resume(&ResumeExecutionRequest {
                run_id: started.run.id,
            })
            .expect("resume after spawn failure");
        wait_for_status(&retry, &blocked.run.id, &["ready"]);
        let builder = runner
            .calls()
            .into_iter()
            .find(|call| {
                Path::new(&call.program)
                    .file_name()
                    .and_then(|name| name.to_str())
                    == Some("copilot")
                    && !call
                        .arguments
                        .iter()
                        .any(|argument| argument == "--deny-tool=write")
            })
            .expect("builder retry");
        assert!(builder
            .arguments
            .iter()
            .any(|argument| argument == "--session-id"));
        assert!(!builder
            .arguments
            .iter()
            .any(|argument| argument == &first_session_id));
        assert!(!builder
            .arguments
            .iter()
            .any(|argument| argument.starts_with("--resume=")));
    }

    #[test]
    fn nonzero_reviewer_launch_keeps_session_fresh_for_retry() {
        let harness = Harness::new(true);
        let first_runner = Arc::new(
            FakeRunner::new(
                [BuilderAction::Write {
                    path: "implemented.txt".to_owned(),
                    content: "implemented\n".to_owned(),
                }],
                [],
            )
            .with_failing_review(),
        );
        let first = harness.service(first_runner);
        let started = start(&first, &harness.queue_entry_id);
        let blocked = wait_for_status(&first, &started.run.id, &["blocked"]);
        assert_eq!(blocked.run.error_code.as_deref(), Some("reviewer_failed"));
        let state: String = harness
            .store
            .with_connection(|connection| {
                connection
                    .query_row(
                        "SELECT reviewer_session_state FROM execution_runs WHERE run_id = ?1",
                        [&started.run.id],
                        |row| row.get(0),
                    )
                    .map_err(Into::into)
            })
            .expect("reviewer state");
        assert_eq!(state, "not_started");

        let retry_runner = Arc::new(FakeRunner::new(
            [],
            [json!({"version": 1, "summary": "No findings.", "findings": []})],
        ));
        let retry = harness.service(retry_runner.clone());
        retry
            .resume(&ResumeExecutionRequest {
                run_id: started.run.id,
            })
            .expect("retry reviewer");
        wait_for_status(&retry, &blocked.run.id, &["ready"]);
        let reviewer = retry_runner
            .calls()
            .into_iter()
            .find(|call| {
                call.arguments
                    .iter()
                    .any(|argument| argument == "--deny-tool=write")
            })
            .expect("reviewer retry");
        assert!(reviewer
            .arguments
            .iter()
            .any(|argument| argument == "--session-id"));
        assert!(!reviewer
            .arguments
            .iter()
            .any(|argument| argument.starts_with("--resume=")));
    }

    #[test]
    fn oversized_complete_evidence_fails_closed_without_truncation() {
        let directory = tempdir().expect("temp dir");
        let root = directory.path();
        let evidence = root.join(".quorum-runtime/evidence");
        fs::create_dir_all(&evidence).expect("evidence directory");
        fs::write(evidence.join("tracked.diff"), "tracked\n").expect("tracked evidence");
        fs::write(evidence.join("untracked.paths"), b"large.bin\0").expect("untracked paths");
        fs::write(
            root.join("large.bin"),
            vec![b'a'; MAX_REVIEW_DIFF_BYTES + 1024],
        )
        .expect("large file");
        let error = collect_base_evidence(
            root,
            &evidence.join("tracked.diff"),
            &evidence.join("untracked.paths"),
        )
        .expect_err("oversized evidence must fail");
        assert_eq!(error.code, "review_evidence_too_large");

        let harness = Harness::new(true);
        let runner = Arc::new(FakeRunner::new(
            [BuilderAction::Write {
                path: "implemented.txt".to_owned(),
                content: "x".repeat(MAX_REVIEW_DIFF_BYTES + 1),
            }],
            [],
        ));
        let service = harness.service(runner.clone());
        let started = start(&service, &harness.queue_entry_id);
        let blocked = wait_for_status(&service, &started.run.id, &["blocked"]);
        assert_eq!(
            blocked.run.error_code.as_deref(),
            Some("review_evidence_too_large")
        );
        assert!(!runner.calls().iter().any(|call| {
            call.arguments
                .iter()
                .any(|argument| argument == "--deny-tool=write")
        }));
    }

    #[test]
    fn cumulative_evidence_io_bound_fails_before_unbounded_hashing() {
        let directory = tempdir().expect("temp dir");
        let root = directory.path();
        let evidence = root.join(".quorum-runtime/evidence");
        fs::create_dir_all(&evidence).expect("evidence directory");
        let tracked = File::create(evidence.join("tracked.diff")).expect("tracked evidence");
        tracked
            .set_len(u64::try_from(MAX_EVIDENCE_BYTES + 1).expect("evidence size"))
            .expect("size tracked evidence");
        fs::write(evidence.join("untracked.paths"), b"").expect("untracked paths");
        let error = collect_base_evidence(
            root,
            &evidence.join("tracked.diff"),
            &evidence.join("untracked.paths"),
        )
        .expect_err("cumulative bound must fail");
        assert_eq!(error.code, "evidence_limit_exceeded");
    }

    #[test]
    fn evidence_hashing_observes_cancellation() {
        let directory = tempdir().expect("temp dir");
        let control = RunControl::default();
        control.cancel();
        let mut budget = EvidenceBudget::new();
        let error =
            collect_base_evidence_bytes(directory.path(), b"", b"", &mut budget, Some(&control))
                .expect_err("cancelled evidence must fail");
        assert_eq!(error.code, "cancelled");
    }

    #[test]
    fn final_evidence_rejects_a_tracked_change_between_complete_captures() {
        let harness = Harness::new(true);
        let service = harness.service(Arc::new(FakeRunner::new([BuilderAction::Fail], [])));
        let started = start(&service, &harness.queue_entry_id);
        let blocked = wait_for_status(&service, &started.run.id, &["blocked"]);
        let snapshot = service.snapshot(&blocked.run.id).expect("worker snapshot");
        let error = ExecutionService::base_evidence_with_interpass(
            &snapshot,
            &RunControl::default(),
            || {
                fs::write(
                    Path::new(&blocked.run.worktree_path).join("Makefile"),
                    "check:\n\t@true\n",
                )
                .expect("mutate tracked content between evidence captures");
            },
        )
        .expect_err("mixed-state evidence must fail closed");
        assert_eq!(error.code, "diff_state_changed");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_untracked_symlinks_without_following_them() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().expect("temp dir");
        let root = directory.path();
        let evidence = root.join(".quorum-runtime/evidence");
        fs::create_dir_all(&evidence).expect("evidence directory");
        fs::write(evidence.join("tracked.diff"), "").expect("tracked evidence");
        fs::write(evidence.join("untracked.paths"), b"link\0").expect("untracked paths");
        let outside = directory.path().join("outside");
        fs::write(&outside, "secret").expect("outside target");
        symlink(&outside, root.join("link")).expect("symlink");
        let error = collect_base_evidence(
            root,
            &evidence.join("tracked.diff"),
            &evidence.join("untracked.paths"),
        )
        .expect_err("symlink must be rejected");
        assert_eq!(error.code, "unsafe_review_file");
    }

    #[cfg(unix)]
    #[test]
    fn full_state_digest_includes_untracked_executable_mode() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempdir().expect("temp dir");
        let root = directory.path();
        let evidence = root.join(".quorum-runtime/evidence");
        fs::create_dir_all(&evidence).expect("evidence directory");
        fs::write(evidence.join("tracked.diff"), "").expect("tracked evidence");
        fs::write(evidence.join("untracked.paths"), b"script\0").expect("untracked paths");
        let script = root.join("script");
        fs::write(&script, "#!/bin/sh\n").expect("script");
        let first = collect_base_evidence(
            root,
            &evidence.join("tracked.diff"),
            &evidence.join("untracked.paths"),
        )
        .expect("first evidence");
        let mut permissions = fs::metadata(&script)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("make executable");
        let second = collect_base_evidence(
            root,
            &evidence.join("tracked.diff"),
            &evidence.join("untracked.paths"),
        )
        .expect("second evidence");
        assert_ne!(first.digest, second.digest);
        assert!(second.review_diff.contains("new file mode 100755"));
    }

    #[test]
    fn full_state_digest_frames_untracked_file_records_unambiguously() {
        let directory = tempdir().expect("temp dir");
        let one_file = directory.path().join("one-file");
        let two_files = directory.path().join("two-files");
        for root in [&one_file, &two_files] {
            fs::create_dir(root).expect("fixture root");
            let evidence = root.join(".quorum-runtime/evidence");
            fs::create_dir_all(&evidence).expect("evidence directory");
            fs::write(evidence.join("tracked.diff"), "").expect("tracked evidence");
        }

        let legacy_record = |path: &[u8], content: &[u8]| {
            let mut record = b"\0untracked-path\0".to_vec();
            record.extend_from_slice(path);
            record.extend_from_slice(b"\0mode\0");
            record.extend_from_slice(b"100644");
            record.extend_from_slice(b"\0content\0");
            record.extend_from_slice(content);
            record
        };
        let mut embedded_second_record = b"first".to_vec();
        embedded_second_record.extend_from_slice(&legacy_record(b"b", b"second"));
        let one_legacy = legacy_record(b"a", &embedded_second_record);
        let mut two_legacy = legacy_record(b"a", b"first");
        two_legacy.extend_from_slice(&legacy_record(b"b", b"second"));
        assert_eq!(
            one_legacy, two_legacy,
            "fixture must collide under the legacy unframed encoding"
        );

        fs::write(
            one_file.join(".quorum-runtime/evidence/untracked.paths"),
            b"a\0",
        )
        .expect("one-file paths");
        fs::write(one_file.join("a"), embedded_second_record).expect("one-file content");
        fs::write(
            two_files.join(".quorum-runtime/evidence/untracked.paths"),
            b"a\0b\0",
        )
        .expect("two-file paths");
        fs::write(two_files.join("a"), b"first").expect("first file");
        fs::write(two_files.join("b"), b"second").expect("second file");

        let one = collect_base_evidence(
            &one_file,
            &one_file.join(".quorum-runtime/evidence/tracked.diff"),
            &one_file.join(".quorum-runtime/evidence/untracked.paths"),
        )
        .expect("one-file evidence");
        let two = collect_base_evidence(
            &two_files,
            &two_files.join(".quorum-runtime/evidence/tracked.diff"),
            &two_files.join(".quorum-runtime/evidence/untracked.paths"),
        )
        .expect("two-file evidence");

        assert_ne!(one.digest, two.digest);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_preexisting_hardlinks_that_could_escape_the_sandbox() {
        let directory = tempdir().expect("temp dir");
        let managed = directory.path().join("managed");
        fs::create_dir(&managed).expect("managed directory");
        let outside = directory.path().join("outside");
        fs::write(&outside, "preserve").expect("outside file");
        fs::hard_link(&outside, managed.join("alias")).expect("hard link");
        let error =
            validate_confinement_tree(&managed).expect_err("hardlink must fail confinement");
        assert_eq!(error.code, "sandbox_unavailable");
        assert_eq!(
            fs::read_to_string(outside).expect("outside content"),
            "preserve"
        );
    }

    #[test]
    fn truncated_reviewer_output_never_reaches_delivery() {
        let harness = Harness::new(true);
        let runner = Arc::new(
            FakeRunner::new(
                [BuilderAction::Write {
                    path: "implemented.txt".to_owned(),
                    content: "implemented\n".to_owned(),
                }],
                [json!({"version": 1, "summary": "No findings.", "findings": []})],
            )
            .with_truncated_reviews(),
        );
        let service = harness.service(runner);
        let started = start(&service, &harness.queue_entry_id);
        let blocked = wait_for_status(&service, &started.run.id, &["blocked"]);
        assert_eq!(
            blocked.run.error_code.as_deref(),
            Some("review_output_incomplete")
        );
        assert!(!blocked.delivery_ready);
    }

    #[test]
    fn run_lease_blocks_overlap_and_owned_wrapper_honors_it() {
        let harness = Harness::new(true);
        let service = harness.service(Arc::new(FakeRunner::new([BuilderAction::Fail], [])));
        let started = start(&service, &harness.queue_entry_id);
        let blocked = wait_for_status(&service, &started.run.id, &["blocked"]);
        let lease_path = harness
            .store
            .run_lease_path(&started.run.id)
            .expect("lease path");
        let lease = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lease_path)
            .expect("lease");
        FileExt::try_lock_exclusive(&lease).expect("own lease");
        let overlap = service
            .resume(&ResumeExecutionRequest {
                run_id: started.run.id,
            })
            .expect("persist overlap diagnostic");
        assert_eq!(
            overlap.run.error_code.as_deref(),
            Some("orphan_process_active")
        );
        assert_eq!(overlap.attempts.len(), blocked.attempts.len());
        let wrapper_error = run_owned_process(vec![
            lease_path.into_os_string(),
            "--".into(),
            "/usr/bin/true".into(),
        ])
        .expect_err("owned wrapper must reject overlap");
        assert_eq!(wrapper_error.kind(), std::io::ErrorKind::WouldBlock);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_profile_allows_only_managed_worktree_writes() {
        let directory = tempdir().expect("temp dir");
        let managed = directory.path().join("managed");
        fs::create_dir(&managed).expect("managed directory");
        let outside = directory.path().join("outside");
        let profile = macos_sandbox_profile(&managed).expect("sandbox profile");
        let inside_status = Command::new("/usr/bin/sandbox-exec")
            .args([
                "-p",
                &profile,
                "/usr/bin/touch",
                managed.join("inside").to_string_lossy().as_ref(),
            ])
            .status()
            .expect("inside sandbox probe");
        assert!(inside_status.success());
        let outside_status = Command::new("/usr/bin/sandbox-exec")
            .args([
                "-p",
                &profile,
                "/usr/bin/touch",
                outside.to_string_lossy().as_ref(),
            ])
            .status()
            .expect("outside sandbox probe");
        assert!(!outside_status.success());
        assert!(!outside.exists());

        let git_pointer = managed.join(".git");
        fs::write(&git_pointer, "gitdir: trusted\n").expect("git pointer");
        let git_status = Command::new("/usr/bin/sandbox-exec")
            .args([
                "-p",
                &profile,
                "/usr/bin/touch",
                git_pointer.to_string_lossy().as_ref(),
            ])
            .status()
            .expect("git pointer sandbox probe");
        assert!(!git_status.success());
        assert_eq!(
            fs::read_to_string(git_pointer).expect("unchanged git pointer"),
            "gitdir: trusted\n"
        );
    }

    #[test]
    fn copilot_home_keeps_package_extraction_inside_the_sandbox() {
        let runtime = Path::new("/managed/.quorum-runtime/builder");
        let environment = copilot_environment(runtime);
        let home = environment
            .iter()
            .find_map(|(name, path)| (*name == "HOME").then_some(path));
        assert_eq!(home, Some(&runtime.join("copilot-home")));
        assert!(home.expect("HOME").starts_with(runtime));
    }

    #[test]
    fn copilot_authentication_prefers_documented_secret_environment_variables() {
        let token = inherited_copilot_token(|name| match name {
            "COPILOT_GITHUB_TOKEN" => Some(String::new()),
            "GH_TOKEN" => Some("github-cli-token".to_owned()),
            "GITHUB_TOKEN" => Some("fallback-token".to_owned()),
            _ => None,
        });
        assert_eq!(token.as_deref(), Some("github-cli-token"));
    }

    #[test]
    fn process_runner_drains_final_output_after_child_exit() {
        let request = ProcessRequest::new(
            "/bin/sh".to_owned(),
            vec![
                "-c".to_owned(),
                "i=0; while [ \"$i\" -lt 2000 ]; do printf 'line-%s\\n' \"$i\"; i=$((i+1)); done; printf FINAL"
                    .to_owned(),
            ],
            PathBuf::from("/"),
        );
        let control = RunControl::default();
        let mut streamed = Vec::new();
        let result = SystemExecutionProcessRunner
            .run(&request, &control, &mut |chunk| {
                if chunk.stream == "stdout" {
                    streamed.extend_from_slice(&chunk.bytes);
                }
            })
            .expect("run child");
        assert!(result.success);
        assert!(result.stdout.ends_with(b"FINAL"));
        assert!(streamed.ends_with(b"FINAL"));
    }

    #[test]
    fn process_runner_retains_completion_evidence_after_capture_truncation() {
        let request = ProcessRequest::new(
            "/bin/sh".to_owned(),
            vec![
                "-c".to_owned(),
                "dd if=/dev/zero bs=600000 count=1 2>/dev/null | tr '\\000' x; printf '\\n{\"type\":\"assistant.message\"}\\n'"
                    .to_owned(),
            ],
            PathBuf::from("/"),
        );
        let result = SystemExecutionProcessRunner
            .run(&request, &RunControl::default(), &mut |_| {})
            .expect("capture process tail");
        assert!(result.success);
        assert!(result.capture_truncated);
        assert!(copilot_session_confirmed(&result));
    }

    #[test]
    fn process_runner_terminates_commands_that_exceed_total_output_budget() {
        let request =
            ProcessRequest::new("/usr/bin/yes".to_owned(), Vec::new(), PathBuf::from("/"));
        let control = RunControl::default();
        let mut streamed = 0_usize;
        let started = Instant::now();
        let result = SystemExecutionProcessRunner
            .run(&request, &control, &mut |chunk| {
                streamed += chunk.bytes.len();
            })
            .expect("observe bounded output failure");
        assert!(!result.success);
        assert!(result.capture_truncated);
        assert!(result.status.contains("process output limit exceeded"));
        assert!(streamed <= MAX_PROCESS_OUTPUT_BYTES);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn bounded_output_backpressure_does_not_deadlock_cancellation() {
        let request =
            ProcessRequest::new("/usr/bin/yes".to_owned(), Vec::new(), PathBuf::from("/"));
        let control = Arc::new(RunControl::default());
        let cancelling = Arc::clone(&control);
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(25));
            cancelling.cancel();
        });
        let started = Instant::now();
        let result = SystemExecutionProcessRunner
            .run(&request, &control, &mut |_| {
                thread::sleep(Duration::from_millis(2));
            })
            .expect("observe cancellation under output backpressure");
        canceller.join().expect("cancellation thread");
        assert!(!result.success);
        assert!(control.cancelled());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    fn process_runner_terminates_descendants_after_direct_child_exit() {
        let directory = tempdir().expect("temp dir");
        let pid_path = directory.path().join("descendant.pid");
        let script = format!(
            "sh -c 'trap \"\" TERM; echo $$ > \"{}\"; while :; do sleep 1; done' &",
            pid_path.display()
        );
        let request = ProcessRequest::new(
            "/bin/sh".to_owned(),
            vec!["-c".to_owned(), script],
            directory.path().to_path_buf(),
        );
        let control = RunControl::default();
        let result = SystemExecutionProcessRunner
            .run(&request, &control, &mut |_| {})
            .expect("run child with descendant");
        assert!(result.success);
        let descendant = fs::read_to_string(&pid_path).expect("descendant pid");
        let status = Command::new("kill")
            .args(["-0", descendant.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("probe descendant");
        assert!(
            !status.success(),
            "background descendant survived run lease"
        );
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_before_child_install_is_not_lost() {
        let control = RunControl::default();
        control.cancel();
        let request = ProcessRequest::new(
            "/bin/sh".to_owned(),
            vec!["-c".to_owned(), "while :; do sleep 1; done".to_owned()],
            PathBuf::from("/"),
        );
        let started = Instant::now();
        let result = SystemExecutionProcessRunner
            .run(&request, &control, &mut |_| {})
            .expect("observe pre-cancelled child");
        assert!(!result.success);
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn multibyte_process_diagnostics_truncate_on_char_boundaries() {
        let result = ProcessResult {
            success: false,
            exit_code: Some(1),
            status: "exit status: 1".to_owned(),
            stdout: Vec::new(),
            stderr: format!("{}étail", "a".repeat(15_999)).into_bytes(),
            capture_truncated: false,
        };
        let evidence = process_evidence(&result);
        assert!(evidence.ends_with("[diagnostic truncated]"));
        assert!(evidence.is_char_boundary(evidence.len()));
    }

    #[test]
    fn truncated_builder_capture_can_confirm_from_the_retained_stdout_tail() {
        let result = ProcessResult {
            success: true,
            exit_code: Some(0),
            status: "exit status: 0".to_owned(),
            stdout: br#"{"type":"assistant.message","data":{"content":"done"}}"#.to_vec(),
            stderr: Vec::new(),
            capture_truncated: true,
        };
        assert!(copilot_session_confirmed(&result));
    }

    #[test]
    fn copilot_logs_keep_complete_messages_and_drop_streaming_deltas() {
        let stdout = br#"{"type":"assistant.tool_call_delta","data":"partial"}
{"type":"assistant.message","data":{"content":"Implemented the requested change."}}
{"type":"error","message":"Actionable failure"}
"#;
        assert_eq!(
            copilot_log_messages(stdout),
            [
                "Implemented the requested change.".to_owned(),
                "Actionable failure".to_owned()
            ]
        );
    }

    #[test]
    fn worker_panic_releases_supervisor_and_blocks_run() {
        let harness = Harness::new(true);
        let supervisor = Arc::new(ExecutionSupervisor::default());
        let service = ExecutionService::with_runner(
            Arc::clone(&harness.store),
            Arc::clone(&supervisor),
            Arc::new(PanicRunner),
        );
        let started = start(&service, &harness.queue_entry_id);
        let blocked = wait_for_status(&service, &started.run.id, &["blocked"]);
        assert_eq!(blocked.run.error_code.as_deref(), Some("worker_panicked"));
        assert!(supervisor.control(&started.run.id).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_kills_the_entire_process_group() {
        let directory = tempdir().expect("temp dir");
        let pid_path = directory.path().join("descendant.pid");
        let script = format!(
            "trap '' TERM; sh -c 'trap \"\" TERM; echo $$ > \"{}\"; while :; do sleep 1; done' & wait",
            pid_path.display()
        );
        let request = ProcessRequest::new(
            "/bin/sh".to_owned(),
            vec!["-c".to_owned(), script],
            directory.path().to_path_buf(),
        );
        let control = Arc::new(RunControl::default());
        let thread_control = Arc::clone(&control);
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = SystemExecutionProcessRunner.run(&request, &thread_control, &mut |_| {});
            sender.send(result).expect("send process result");
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        while !pid_path.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let descendant = fs::read_to_string(&pid_path).expect("descendant pid");
        control.cancel();
        receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("runner must not hang after cancellation")
            .expect("observe cancelled child");
        let status = Command::new("kill")
            .args(["-0", descendant.trim()])
            .status()
            .expect("probe descendant");
        assert!(
            !status.success(),
            "descendant process survived cancellation"
        );
    }
}
