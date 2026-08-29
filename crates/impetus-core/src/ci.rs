//! Minimal GitLab CI frontends. They render an existing `.gitlab-ci.yml`; they
//! never become a scheduler or a second CI configuration format.

use serde_json::Value;
use std::{
    ffi::OsStr,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineStatus {
    Success,
    Failed,
    Running,
    Pending,
    Canceled,
    Skipped,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Success,
    Failed,
    Running,
    Pending,
    Skipped,
    Canceled,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pipeline {
    pub id: Option<String>,
    pub branch: String,
    pub status: PipelineStatus,
    pub stages: Vec<Stage>,
    pub duration: Option<Duration>,
    pub web_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage {
    pub name: String,
    pub jobs: Vec<Job>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: Option<String>,
    pub name: String,
    pub status: JobStatus,
    pub duration: Option<Duration>,
    pub log: Option<String>,
    pub error_summary: Option<String>,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiProject {
    pub workspace: PathBuf,
    pub branch: String,
    pub remote: Option<String>,
}

#[derive(Debug, Error)]
pub enum CiError {
    #[error("`.gitlab-ci.yml` was not found in `{0}`")]
    MissingConfiguration(PathBuf),
    #[error("the repository has no GitLab origin remote")]
    MissingGitLabRemote,
    #[error("could not start `{program}`: {source}")]
    CommandStart {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error("`{program}` exited unsuccessfully: {summary}")]
    CommandFailed { program: String, summary: String },
    #[error("`{command}` returned invalid JSON: {source}")]
    InvalidJson {
        command: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("GitLab returned no pipeline for the current branch")]
    PipelineNotFound,
    #[error("CI action `{0}` is not part of the v0.2 CI slice")]
    UnsupportedAction(&'static str),
}

/// Small seam for the UI. Remote mutation actions deliberately stay out of the
/// v0.2 CI slice; `retry` and `cancel` require their own approval UX.
pub trait CiBackend {
    fn detect(&self, workspace: &Path) -> Result<CiProject, CiError>;
    fn run(&self, _project: &CiProject) -> Result<Pipeline, CiError> {
        Err(CiError::UnsupportedAction("run"))
    }
    fn status(&self, project: &CiProject) -> Result<Pipeline, CiError>;
    fn jobs(&self, project: &CiProject, pipeline_id: &str) -> Result<Vec<Stage>, CiError>;
    fn logs(&self, project: &CiProject, job_id: &str) -> Result<String, CiError>;
    fn retry(&self, _project: &CiProject, _job_id: &str) -> Result<(), CiError> {
        Err(CiError::UnsupportedAction("retry"))
    }
    fn cancel(&self, _project: &CiProject, _job_id: &str) -> Result<(), CiError> {
        Err(CiError::UnsupportedAction("cancel"))
    }
}

#[derive(Debug, Clone, Default)]
pub struct LocalGitlabBackend;

pub struct LocalRun {
    pub pipeline: Pipeline,
    receiver: Receiver<LocalCiEvent>,
}

impl LocalRun {
    pub fn try_next(&self) -> Result<Option<LocalCiEvent>, CiError> {
        match self.receiver.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Ok(None),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalCiEvent {
    Output(String),
    Completed {
        succeeded: bool,
        exit_code: Option<i32>,
        duration: Duration,
    },
    Failed(String),
}

impl LocalGitlabBackend {
    pub fn start(&self, workspace: &Path) -> Result<LocalRun, CiError> {
        let project = self.detect(workspace)?;
        let mut pipeline = self.status(&project)?;
        pipeline.status = PipelineStatus::Running;

        let workspace = project.workspace;
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || stream_local_run(workspace, sender));
        Ok(LocalRun { pipeline, receiver })
    }

    pub fn run(&self, workspace: &Path) -> Result<Pipeline, CiError> {
        let run = self.start(workspace)?;
        let mut pipeline = run.pipeline;
        let mut log = String::new();
        while let Ok(event) = run.receiver.recv() {
            match event {
                LocalCiEvent::Output(line) => {
                    log.push_str(&line);
                    log.push('\n');
                    apply_local_output(&mut pipeline, &line);
                }
                LocalCiEvent::Completed {
                    succeeded,
                    exit_code,
                    duration,
                } => {
                    pipeline.duration = Some(duration);
                    finalize_local_run(&mut pipeline, succeeded, exit_code, &log);
                }
                LocalCiEvent::Failed(message) => {
                    return Err(CiError::CommandFailed {
                        program: "gitlab-ci-local".into(),
                        summary: message,
                    });
                }
            }
        }
        Ok(pipeline)
    }
}

impl CiBackend for LocalGitlabBackend {
    fn detect(&self, workspace: &Path) -> Result<CiProject, CiError> {
        require_ci_file(workspace)?;
        Ok(CiProject {
            workspace: workspace.to_path_buf(),
            branch: current_branch(workspace).unwrap_or_else(|_| "local".into()),
            remote: None,
        })
    }

    fn run(&self, project: &CiProject) -> Result<Pipeline, CiError> {
        LocalGitlabBackend::run(self, &project.workspace)
    }

    fn status(&self, project: &CiProject) -> Result<Pipeline, CiError> {
        let result = run_command("gitlab-ci-local", ["--list-csv-all"], &project.workspace)?;
        if !result.success() {
            return Err(command_failed("gitlab-ci-local", &result));
        }
        Ok(Pipeline {
            id: None,
            branch: project.branch.clone(),
            status: PipelineStatus::Pending,
            stages: parse_local_job_list(&result.stdout),
            duration: None,
            web_url: None,
        })
    }

    fn jobs(&self, project: &CiProject, _pipeline_id: &str) -> Result<Vec<Stage>, CiError> {
        Ok(self.status(project)?.stages)
    }

    fn logs(&self, _project: &CiProject, _job_id: &str) -> Result<String, CiError> {
        Err(CiError::CommandFailed {
            program: "gitlab-ci-local".into(),
            summary: "local logs are captured only for the active run".into(),
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct RemoteGitlabBackend;

impl CiBackend for RemoteGitlabBackend {
    fn detect(&self, workspace: &Path) -> Result<CiProject, CiError> {
        require_ci_file(workspace)?;
        let remote = command_stdout("git", ["remote", "get-url", "origin"], workspace)?;
        if !is_gitlab_remote(&remote) {
            return Err(CiError::MissingGitLabRemote);
        }
        Ok(CiProject {
            workspace: workspace.to_path_buf(),
            branch: current_branch(workspace)?,
            remote: Some(remote.trim().into()),
        })
    }

    fn status(&self, project: &CiProject) -> Result<Pipeline, CiError> {
        let output = command_stdout(
            "glab",
            [
                "ci",
                "list",
                "--ref",
                project.branch.as_str(),
                "--per-page",
                "1",
                "--output",
                "json",
            ],
            &project.workspace,
        )?;
        let records: Value =
            serde_json::from_str(&output).map_err(|source| CiError::InvalidJson {
                command: "glab ci list --output json".into(),
                source,
            })?;
        let record = records
            .as_array()
            .and_then(|pipelines| pipelines.first())
            .ok_or(CiError::PipelineNotFound)?;
        let id = json_string(record, "id").ok_or(CiError::PipelineNotFound)?;
        let mut pipeline = Pipeline {
            id: Some(id.clone()),
            branch: json_string(record, "ref").unwrap_or_else(|| project.branch.clone()),
            status: pipeline_status(record.get("status").and_then(Value::as_str)),
            stages: self.jobs(project, &id)?,
            duration: json_duration(record, "duration"),
            web_url: json_string(record, "web_url"),
        };
        if pipeline.status == PipelineStatus::Unknown {
            pipeline.status =
                pipeline_status(record.get("detailed_status").and_then(Value::as_str));
        }
        Ok(pipeline)
    }

    fn jobs(&self, project: &CiProject, pipeline_id: &str) -> Result<Vec<Stage>, CiError> {
        let endpoint = format!("projects/:fullpath/pipelines/{pipeline_id}/jobs?per_page=100");
        let output = command_stdout(
            "glab",
            ["api", "--paginate", "--output", "json", endpoint.as_str()],
            &project.workspace,
        )?;
        let records: Value =
            serde_json::from_str(&output).map_err(|source| CiError::InvalidJson {
                command: "glab api …/jobs --output json".into(),
                source,
            })?;
        let mut stages = Vec::new();
        for record in records.as_array().into_iter().flatten() {
            let stage_name = json_string(record, "stage").unwrap_or_else(|| "other".into());
            let job = Job {
                id: json_string(record, "id"),
                name: json_string(record, "name").unwrap_or_else(|| "unnamed job".into()),
                status: job_status(record.get("status").and_then(Value::as_str)),
                duration: json_duration(record, "duration"),
                log: None,
                error_summary: json_string(record, "failure_reason"),
                exit_code: None,
            };
            push_job(&mut stages, stage_name, job);
        }
        Ok(stages)
    }

    fn logs(&self, project: &CiProject, job_id: &str) -> Result<String, CiError> {
        let endpoint = format!("projects/:fullpath/jobs/{job_id}/trace");
        command_stdout("glab", ["api", endpoint.as_str()], &project.workspace)
    }
}

pub fn parse_local_job_list(source: &str) -> Vec<Stage> {
    let mut stages = Vec::new();
    for line in source
        .lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
    {
        let mut fields = line.split(';');
        let Some(name) = fields.next().filter(|value| !value.trim().is_empty()) else {
            continue;
        };
        let stage = fields.next().unwrap_or("other").trim();
        let when = fields.next().unwrap_or_default().trim();
        push_job(
            &mut stages,
            stage.into(),
            Job {
                id: None,
                name: name.trim().into(),
                status: if when == "never" {
                    JobStatus::Skipped
                } else {
                    JobStatus::Pending
                },
                duration: None,
                log: None,
                error_summary: None,
                exit_code: None,
            },
        );
    }
    stages
}

pub fn extract_error_summary(log: &str) -> Option<String> {
    let lines: Vec<_> = log.lines().collect();
    let index = lines.iter().position(is_error_line)?;
    let start = index.saturating_sub(2);
    let end = (index + 4).min(lines.len());
    Some(lines[start..end].join("\n"))
}

pub fn apply_local_output(pipeline: &mut Pipeline, line: &str) {
    let Some((stage_index, job_index)) = find_job_in_line(pipeline, line) else {
        return;
    };
    for stage in &mut pipeline.stages {
        for job in &mut stage.jobs {
            if job.status == JobStatus::Running {
                job.status = JobStatus::Success;
            }
        }
    }
    pipeline.stages[stage_index].jobs[job_index].status = JobStatus::Running;
}

pub fn finalize_local_run(
    pipeline: &mut Pipeline,
    succeeded: bool,
    exit_code: Option<i32>,
    full_log: &str,
) {
    pipeline.status = if succeeded {
        PipelineStatus::Success
    } else {
        PipelineStatus::Failed
    };
    let summary = (!succeeded)
        .then(|| extract_error_summary(full_log))
        .flatten();
    let mut active = None;
    for (stage_index, stage) in pipeline.stages.iter_mut().enumerate() {
        for (job_index, job) in stage.jobs.iter_mut().enumerate() {
            if job.status == JobStatus::Running {
                active = Some((stage_index, job_index));
            }
        }
    }
    if let Some((stage_index, job_index)) = active {
        let job = &mut pipeline.stages[stage_index].jobs[job_index];
        job.status = if succeeded {
            JobStatus::Success
        } else {
            JobStatus::Failed
        };
        job.exit_code = exit_code;
        job.error_summary = summary;
    }
}

fn stream_local_run(workspace: PathBuf, sender: Sender<LocalCiEvent>) {
    let mut child = match Command::new("gitlab-ci-local")
        .current_dir(workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let _ = sender.send(LocalCiEvent::Failed(error.to_string()));
            return;
        }
    };
    let started = Instant::now();
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let saw_failure = Arc::new(AtomicBool::new(false));
    let stdout_reader = spawn_line_reader(stdout, sender.clone(), saw_failure.clone());
    let stderr_reader = spawn_line_reader(stderr, sender.clone(), saw_failure.clone());
    let result = child.wait();
    let _ = stdout_reader.join();
    let _ = stderr_reader.join();
    match result {
        Ok(status) => {
            let _ = sender.send(LocalCiEvent::Completed {
                // gitlab-ci-local can report a failed job in its final summary
                // while returning exit code 0. Treat the runner's explicit FAIL
                // marker as a failure too; a false positive here fails closed.
                succeeded: status.success() && !saw_failure.load(Ordering::Relaxed),
                exit_code: status.code(),
                duration: started.elapsed(),
            });
        }
        Err(error) => {
            let _ = sender.send(LocalCiEvent::Failed(error.to_string()));
        }
    }
}

fn spawn_line_reader<R: std::io::Read + Send + 'static>(
    reader: R,
    sender: Sender<LocalCiEvent>,
    saw_failure: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            if is_local_failure_marker(&line) {
                saw_failure.store(true, Ordering::Relaxed);
            }
            let _ = sender.send(LocalCiEvent::Output(strip_ansi(&line)));
        }
    })
}

fn strip_ansi(source: &str) -> String {
    let mut result = String::with_capacity(source.len());
    let mut characters = source.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\u{1b}' && characters.peek() == Some(&'[') {
            let _ = characters.next();
            for control in characters.by_ref() {
                if control.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            result.push(character);
        }
    }
    result
}

fn find_job_in_line(pipeline: &Pipeline, line: &str) -> Option<(usize, usize)> {
    let line = line.to_lowercase();
    pipeline
        .stages
        .iter()
        .enumerate()
        .find_map(|(stage_index, stage)| {
            stage.jobs.iter().enumerate().find_map(|(job_index, job)| {
                line.contains(&job.name.to_lowercase())
                    .then_some((stage_index, job_index))
            })
        })
}

fn is_error_line(line: &&str) -> bool {
    let normalized = line.to_lowercase();
    normalized.contains("error[")
        || normalized.contains("error:")
        || normalized.contains("fatal:")
        || normalized.contains("panic")
        || normalized.contains("failed")
        || normalized.contains("assertionerror")
        || normalized.contains("command exited with")
}

fn is_local_failure_marker(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("FAIL ") || (line.contains(" finished in ") && line.contains(" FAIL "))
}

fn require_ci_file(workspace: &Path) -> Result<(), CiError> {
    if workspace.join(".gitlab-ci.yml").is_file() {
        Ok(())
    } else {
        Err(CiError::MissingConfiguration(workspace.to_path_buf()))
    }
}

fn current_branch(workspace: &Path) -> Result<String, CiError> {
    command_stdout("git", ["branch", "--show-current"], workspace)
        .map(|branch| branch.trim().into())
}

fn is_gitlab_remote(remote: &str) -> bool {
    let remote = remote.trim().to_lowercase();
    remote.contains("gitlab.") || remote.starts_with("git@gitlab:")
}

fn run_command<I, S>(program: &str, args: I, cwd: &Path) -> Result<CommandResult, CiError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|source| CiError::CommandStart {
            program: program.into(),
            source,
        })?;
    Ok(CommandResult {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn command_stdout<I, S>(program: &str, args: I, cwd: &Path) -> Result<String, CiError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let result = run_command(program, args, cwd)?;
    if result.success() {
        Ok(result.stdout)
    } else {
        Err(command_failed(program, &result))
    }
}

struct CommandResult {
    success: bool,
    stdout: String,
    stderr: String,
}

impl CommandResult {
    fn success(&self) -> bool {
        self.success
    }
}

fn command_failed(program: &str, result: &CommandResult) -> CiError {
    CiError::CommandFailed {
        program: program.into(),
        summary: extract_error_summary(&result.stderr)
            .or_else(|| extract_error_summary(&result.stdout))
            .unwrap_or_else(|| "no usable error fragment was produced".into()),
    }
}

fn push_job(stages: &mut Vec<Stage>, stage_name: String, job: Job) {
    if let Some(stage) = stages.iter_mut().find(|stage| stage.name == stage_name) {
        stage.jobs.push(job);
    } else {
        stages.push(Stage {
            name: stage_name,
            jobs: vec![job],
        });
    }
}

fn json_string(record: &Value, field: &str) -> Option<String> {
    record.get(field).and_then(|value| match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    })
}

fn json_duration(record: &Value, field: &str) -> Option<Duration> {
    record
        .get(field)
        .and_then(Value::as_f64)
        .filter(|seconds| *seconds >= 0.0)
        .map(Duration::from_secs_f64)
}

fn pipeline_status(status: Option<&str>) -> PipelineStatus {
    match status.map(str::to_ascii_lowercase).as_deref() {
        Some("success") => PipelineStatus::Success,
        Some("failed") => PipelineStatus::Failed,
        Some("running") => PipelineStatus::Running,
        Some("pending") | Some("created") | Some("preparing") | Some("waiting_for_resource") => {
            PipelineStatus::Pending
        }
        Some("canceled") | Some("cancelled") => PipelineStatus::Canceled,
        Some("skipped") => PipelineStatus::Skipped,
        _ => PipelineStatus::Unknown,
    }
}

fn job_status(status: Option<&str>) -> JobStatus {
    match status.map(str::to_ascii_lowercase).as_deref() {
        Some("success") => JobStatus::Success,
        Some("failed") => JobStatus::Failed,
        Some("running") => JobStatus::Running,
        Some("pending")
        | Some("created")
        | Some("preparing")
        | Some("manual")
        | Some("waiting_for_resource") => JobStatus::Pending,
        Some("skipped") => JobStatus::Skipped,
        Some("canceled") | Some("cancelled") => JobStatus::Canceled,
        _ => JobStatus::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_csv_preserves_stage_order_and_never_jobs() {
        let stages = parse_local_job_list(
            "name;stage;when;allowFailure;needs\nfmt;test;on_success;false;\npackage;build;never;false;\nclippy;test;on_success;false;\n",
        );
        assert_eq!(stages.len(), 2);
        assert_eq!(stages[0].name, "test");
        assert_eq!(stages[0].jobs[1].name, "clippy");
        assert_eq!(stages[1].jobs[0].status, JobStatus::Skipped);
    }

    #[test]
    fn error_summary_keeps_compiler_context() {
        let log = "compile\n  --> src/session.rs:142:18\nerror[E0308]: mismatched types\nexpected `Session`\n   found `Option<Session>`\nfinished";
        let summary = extract_error_summary(log).expect("summary");
        assert!(summary.contains("error[E0308]"));
        assert!(summary.contains("found `Option<Session>`"));
    }

    #[test]
    fn error_summary_supports_node_shell_and_generic_stderr() {
        for log in [
            "npm ERR! fatal: install failed",
            "AssertionError: expected true",
            "command exited with 1",
            "thread 'main' panicked",
        ] {
            assert!(extract_error_summary(log).is_some(), "{log}");
        }
    }

    #[test]
    fn gitlab_remote_accepts_https_and_ssh_forms() {
        assert!(is_gitlab_remote(
            "https://gitlab.example.test/group/project.git"
        ));
        assert!(is_gitlab_remote("git@gitlab:group/project.git"));
        assert!(!is_gitlab_remote("https://github.com/group/project.git"));
    }

    #[test]
    fn remote_status_mapping_is_conservative() {
        assert_eq!(
            pipeline_status(Some("waiting_for_resource")),
            PipelineStatus::Pending
        );
        assert_eq!(job_status(Some("manual")), JobStatus::Pending);
        assert_eq!(pipeline_status(Some("new_state")), PipelineStatus::Unknown);
    }

    #[test]
    fn final_local_failure_marks_only_the_active_job() {
        let mut pipeline = Pipeline {
            id: None,
            branch: "local".into(),
            status: PipelineStatus::Running,
            stages: parse_local_job_list(
                "name;stage;when\nfmt;test;on_success\ncompile;build;on_success\n",
            ),
            duration: None,
            web_url: None,
        };
        apply_local_output(&mut pipeline, "running compile");
        finalize_local_run(&mut pipeline, false, Some(1), "error: compile failed");
        assert_eq!(pipeline.status, PipelineStatus::Failed);
        assert_eq!(pipeline.stages[1].jobs[0].status, JobStatus::Failed);
        assert!(pipeline.stages[1].jobs[0].error_summary.is_some());
    }

    #[test]
    fn local_runner_failure_marker_wins_over_zero_exit_code() {
        assert!(is_local_failure_marker(
            "compile finished in 5.51 ms  FAIL 1"
        ));
        assert!(is_local_failure_marker(" FAIL  compile"));
        assert!(!is_local_failure_marker(" PASS  fmt"));
    }
}
