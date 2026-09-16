//! The jobs this workspace handed to Carrick Cloud, and what it can ask about
//! them (carrick#1229).
//!
//! A dispatched scan ends on the laptop before the index exists. What survives
//! it is one file — `.carrick/jobs.json` — naming each repo's job, and two
//! reads of the cloud: how far a job has got, and where its answers are. Those
//! are the only network calls any local command makes outside `index` and
//! `refresh`, and they happen only when a job is recorded here, so the
//! offline path is untouched.
//!
//! The record outlives the scan record beside it. `.carrick/scan-<id>.json` is
//! cleared by the next build, and a user who dispatches, closes the laptop and
//! comes back on Monday must still be able to run `carrick resume`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::credentials::{API_BASE, Credential};

/// The file, beside the index it does not have yet.
const JOBS_FILE: &str = "jobs.json";

/// The tag the file is written under.
const JOBS_SCHEMA: &str = "carrick.jobs/0";

/// How long either network read may take before the local answer stands on its
/// own. A status read is one DynamoDB lookup; a user waiting on `carrick
/// status` is not waiting on the cloud.
const READ_TIMEOUT: Duration = Duration::from_secs(20);

/// One repo's dispatched job.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// `owner/repo`, as the cloud knows it.
    pub repo: String,
    /// Where that repo is on this machine, so a resume knows which tree to
    /// rebuild the prompts from.
    pub path: String,
    pub job_id: String,
    /// The commit the prompts were built at. The join is by content, so this
    /// is not a condition of resuming — it is what the resume says it is
    /// comparing against.
    pub commit: String,
    pub analyze_rows: usize,
    /// RFC 3339.
    pub submitted_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_seconds: Option<u64>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct JobsFile {
    schema: String,
    jobs: Vec<Job>,
}

pub fn jobs_file(index_dir: &Path) -> PathBuf {
    index_dir.join(JOBS_FILE)
}

/// Every job this workspace is waiting on. Empty when the file is absent or
/// unreadable: a record nobody can read is a record of nothing, and no command
/// should fail because of it.
pub fn read(index_dir: &Path) -> Vec<Job> {
    std::fs::read(jobs_file(index_dir))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<JobsFile>(&bytes).ok())
        .map(|file| file.jobs)
        .unwrap_or_default()
}

/// Record a job, replacing whatever this repo had recorded before: one repo
/// waits on one job, and a second dispatch of the same repo supersedes the
/// first rather than queueing behind it.
///
/// Keyed on the path rather than the name. The path is what a resume needs and
/// what this machine can be sure is one repo; a name is what a tree with no
/// remote has to be given.
pub fn record(index_dir: &Path, job: Job) -> Result<(), String> {
    let mut jobs = read(index_dir);
    jobs.retain(|existing| existing.path != job.path);
    jobs.push(job);
    write(index_dir, jobs)
}

/// Forget the jobs whose answers have been collected.
pub fn forget(index_dir: &Path, collected: &[String]) -> Result<(), String> {
    let jobs: Vec<Job> = read(index_dir)
        .into_iter()
        .filter(|job| !collected.contains(&job.job_id))
        .collect();
    write(index_dir, jobs)
}

fn write(index_dir: &Path, jobs: Vec<Job>) -> Result<(), String> {
    let path = jobs_file(index_dir);
    if jobs.is_empty() {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("{}: {error}", path.display())),
        };
    }
    let _ = std::fs::create_dir_all(index_dir);
    let file = JobsFile {
        schema: JOBS_SCHEMA.to_string(),
        jobs,
    };
    let text = serde_json::to_vec_pretty(&file).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("{}: {error}", path.display(), error = e))
}

/// How far a job has got, as the cloud says it.
///
/// Every field is optional on the wire: this is read by `carrick status`,
/// which must answer something whatever the cloud says, and a status line that
/// failed to parse is worse than one that says less.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct JobStatus {
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub done: usize,
    #[serde(default)]
    pub total: usize,
    #[serde(default)]
    pub eta_seconds: Option<u64>,
    /// When the answers stop being available to download. The answers
    /// themselves are content-addressed and outlive it, so this is about one
    /// download, not about the work.
    #[serde(default)]
    pub expires_at: Option<String>,
}

impl JobStatus {
    /// Whether there is something to collect.
    pub fn is_ready(&self) -> bool {
        self.state == "ready" || self.state == "partial"
    }

    pub fn has_failed(&self) -> bool {
        self.state == "failed"
    }

    /// How far through, when the cloud gave both halves of the fraction.
    pub fn percent(&self) -> Option<usize> {
        (self.total > 0).then(|| self.done * 100 / self.total)
    }
}

/// The two reads a local command makes about a job.
///
/// Modelled on the hosted reader beside it: one client, no default
/// credentials, no redirects, and a staged URL is checked before it is
/// followed.
pub struct JobReader {
    client: reqwest::Client,
    endpoint: String,
}

impl JobReader {
    pub fn new() -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(READ_TIMEOUT)
            .connect_timeout(Duration::from_secs(10))
            .gzip(true)
            .build()
            .map_err(|_| "Could not reach Carrick Cloud")?;
        Ok(Self {
            client,
            endpoint: format!("{API_BASE}/types/check-or-upload"),
        })
    }

    async fn post(
        &self,
        credential: &Credential,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&credential.token)
            .json(&body)
            .send()
            .await
            .map_err(|_| "Could not reach Carrick Cloud")?;
        let status = response.status();
        if !status.is_success() {
            return Err(if status == reqwest::StatusCode::UNAUTHORIZED {
                "Carrick Cloud did not accept this machine's credential. Run carrick login.".into()
            } else {
                format!("Carrick Cloud answered HTTP {}", status.as_u16())
            });
        }
        response
            .json()
            .await
            .map_err(|_| "Carrick Cloud sent an answer this version cannot read".into())
    }

    /// How far a job has got.
    pub async fn status(&self, credential: &Credential, job: &Job) -> Result<JobStatus, String> {
        let value = self
            .post(
                credential,
                serde_json::json!({"action": "analysis-job-status", "job_id": job.job_id}),
            )
            .await?;
        serde_json::from_value(value)
            .map_err(|_| "Carrick Cloud sent a job status this version cannot read".to_string())
    }

    /// Download the answers, returning where they were written.
    ///
    /// To a file rather than into memory: the bundle is large, the scan
    /// subprocess is the thing that reads it, and a path is what crosses that
    /// boundary.
    pub async fn answers(
        &self,
        credential: &Credential,
        job: &Job,
        into: &Path,
    ) -> Result<PathBuf, String> {
        let value = self
            .post(
                credential,
                serde_json::json!({"action": "analysis-job-answers", "job_id": job.job_id}),
            )
            .await?;
        let url = value
            .get("url")
            .and_then(|url| url.as_str())
            .ok_or("Carrick Cloud did not say where this job's answers are")?;
        let url = reqwest::Url::parse(url).map_err(|_| "Carrick Cloud sent an unusable link")?;
        // The same three conditions the hosted read applies to a staged URL: a
        // link this machine follows with no question asked must be https, must
        // carry no credentials of its own, and must name a host.
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.host_str().is_none()
        {
            return Err("Carrick Cloud sent an unusable link".to_string());
        }
        let bytes = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| "Could not download the analysis")?
            .bytes()
            .await
            .map_err(|_| "Could not download the analysis")?;
        let _ = std::fs::create_dir_all(into);
        let path = into.join(format!("answers-{}.ndjson.gz", job.job_id));
        std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(path)
    }
}

/// Ask the cloud how far each job has got.
///
/// One answer per job, in the order they were given, and a failed read is that
/// job's answer rather than the whole command's: `carrick status` says what it
/// knows locally either way, and a laptop on a train is not an error.
///
/// A thread of its own with its own runtime, like the hosted reader beside it:
/// the CLI dispatcher is already inside Tokio.
pub fn ask(jobs: &[Job]) -> Vec<Result<JobStatus, String>> {
    let credential = match Credential::load() {
        Ok(Some(credential)) => credential,
        Ok(None) => {
            return jobs.iter().map(|_| Err(SIGNED_OUT.to_string())).collect();
        }
        Err(error) => return jobs.iter().map(|_| Err(error.clone())).collect(),
    };
    let asked: Vec<Job> = jobs.to_vec();
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => return asked.iter().map(|_| Err(UNREACHABLE.to_string())).collect(),
        };
        let reader = match JobReader::new() {
            Ok(reader) => reader,
            Err(error) => return asked.iter().map(|_| Err(error.clone())).collect(),
        };
        runtime.block_on(async {
            let mut answers = Vec::with_capacity(asked.len());
            for job in &asked {
                answers.push(reader.status(&credential, job).await);
            }
            answers
        })
    })
    .join()
    .unwrap_or_else(|_| jobs.iter().map(|_| Err(UNREACHABLE.to_string())).collect())
}

/// Download one job's answers, and say where they landed.
pub fn download(job: &Job, into: &Path) -> Result<PathBuf, String> {
    let credential = Credential::load()?.ok_or(SIGNED_OUT)?;
    let job = job.clone();
    let into = into.to_path_buf();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| UNREACHABLE.to_string())?;
        let reader = JobReader::new()?;
        runtime.block_on(reader.answers(&credential, &job, &into))
    })
    .join()
    .unwrap_or_else(|_| Err(UNREACHABLE.to_string()))
}

const SIGNED_OUT: &str = "this machine is not signed in to Carrick Cloud. Run carrick login.";
const UNREACHABLE: &str = "Could not reach Carrick Cloud";

/// How long ago, in the words a line uses: `12 minutes`, `6 days`.
pub fn since(timestamp: &str) -> Option<String> {
    let at = chrono::DateTime::parse_from_rfc3339(timestamp).ok()?;
    let seconds = (chrono::Utc::now() - at.with_timezone(&chrono::Utc)).num_seconds();
    Some(plural(seconds))
}

/// How long until a timestamp in the future, in the same units. `None` for one
/// that has passed: a window that has closed is not a window.
pub fn until(timestamp: &str) -> Option<String> {
    let at = chrono::DateTime::parse_from_rfc3339(timestamp).ok()?;
    let seconds = (at.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_seconds();
    (seconds > 0).then(|| plural(seconds))
}

/// The same units, for a duration the cloud states rather than one measured
/// from a timestamp.
pub fn duration(seconds: u64) -> String {
    plural(seconds as i64)
}

fn plural(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let (count, unit) = match seconds {
        s if s < 90 => (1, "minute"),
        s if s < 5400 => (s / 60, "minute"),
        s if s < 172_800 => (s / 3600, "hour"),
        s => (s / 86_400, "day"),
    };
    format!("{count} {unit}{}", if count == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(repo: &str, id: &str) -> Job {
        Job {
            repo: repo.to_string(),
            path: format!("/w/{repo}"),
            job_id: id.to_string(),
            commit: "abc1234".to_string(),
            analyze_rows: 10,
            submitted_at: "2026-09-16T10:00:00Z".to_string(),
            eta_seconds: Some(600),
        }
    }

    #[test]
    fn a_job_survives_the_command_that_dispatched_it() {
        let dir = tempfile::tempdir().unwrap();
        record(dir.path(), job("owner/api", "j1")).unwrap();
        record(dir.path(), job("owner/web", "j2")).unwrap();
        let read_back = read(dir.path());
        assert_eq!(read_back.len(), 2);
        assert_eq!(read_back[0].job_id, "j1");
    }

    /// One repo waits on one job. A second dispatch replaces the first rather
    /// than leaving a reader two jobs to choose between.
    #[test]
    fn a_second_dispatch_of_one_repo_replaces_its_job() {
        let dir = tempfile::tempdir().unwrap();
        record(dir.path(), job("owner/api", "j1")).unwrap();
        record(dir.path(), job("owner/api", "j2")).unwrap();
        // Two trees whose remotes name them the same thing are still two jobs:
        // the path is the key.
        let read_back = read(dir.path());
        assert_eq!(read_back.len(), 1);
        assert_eq!(read_back[0].job_id, "j2");
    }

    #[test]
    fn collecting_every_job_removes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        record(dir.path(), job("owner/api", "j1")).unwrap();
        forget(dir.path(), &["j1".to_string()]).unwrap();
        assert!(read(dir.path()).is_empty());
        assert!(!jobs_file(dir.path()).exists());
    }

    #[test]
    fn an_unreadable_record_is_a_record_of_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(jobs_file(dir.path()), "{not json").unwrap();
        assert!(read(dir.path()).is_empty());
    }

    #[test]
    fn a_status_says_whether_there_is_anything_to_collect() {
        let running: JobStatus = serde_json::from_value(
            serde_json::json!({"state": "running", "done": 8570, "total": 13389}),
        )
        .unwrap();
        assert!(!running.is_ready());
        assert_eq!(running.percent(), Some(64));

        let ready: JobStatus =
            serde_json::from_value(serde_json::json!({"state": "ready"})).unwrap();
        assert!(ready.is_ready());
        assert_eq!(ready.percent(), None, "a fraction nobody sent is not shown");

        let partial: JobStatus =
            serde_json::from_value(serde_json::json!({"state": "partial"})).unwrap();
        assert!(partial.is_ready(), "what landed is still worth collecting");

        let unknown: JobStatus = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(!unknown.is_ready() && !unknown.has_failed());
    }

    #[test]
    fn durations_read_as_a_person_would_say_them() {
        assert_eq!(duration(30), "1 minute");
        assert_eq!(duration(600), "10 minutes");
        assert_eq!(duration(7200), "2 hours");
        assert_eq!(duration(518_400), "6 days");
    }
}
