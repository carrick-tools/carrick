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
///
/// Read outside this binary: `scripts/dispatch-smoke.sh` asserts a dispatch
/// happened by reading this file's `job_id`, `repo` and `analyze_rows`
/// (carrick#1259).
const JOBS_SCHEMA: &str = "carrick.jobs/0";

/// How long the status read may take before the local answer stands on its
/// own.
///
/// Short on purpose. `carrick status` is what the session-start hook renders,
/// so this is on the path of every editor open once a job is recorded, and a
/// job status is one lookup. A machine with no network must cost the reader a
/// few seconds, not half a minute.
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the download may take. Not the same budget: this is the object a
/// resume is for, and the command that asks for it is doing nothing else.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);

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
/// The field names are the deployed ones (`jobStatusBody` in `carrick-cloud`
/// `lambdas/check-or-upload/analysis_job.js`). Every one is optional on the
/// wire: this is read by `carrick status`, which must answer something
/// whatever the cloud says, and a status line that failed to parse is worse
/// than one that says less.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct JobStatus {
    /// `queued`, `running`, `ready`, `partial`, `failed` or `cancelled`. The
    /// last four are terminal.
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub total_rows: usize,
    #[serde(default)]
    pub answered: usize,
    /// The cloud's own figure, rather than one derived from two counts that
    /// can disagree with it.
    #[serde(default)]
    pub percent: Option<usize>,
    /// Why a job stopped, when it stopped: `driver_stopped` for one whose
    /// driver is no longer renewing.
    #[serde(default)]
    pub failure_reason: Option<String>,
    /// When the answers stop being available to download. The answers
    /// themselves are content-addressed and outlive it, so this is about one
    /// download, not about the work.
    #[serde(default)]
    pub expires_at: Option<String>,
    /// Which job this is. Read only when the status was asked for by REPO:
    /// the cloud follows its own repo -> job pointer, so this names the job a
    /// workspace that lost `jobs.json` has no other way of learning
    /// (carrick#1320).
    #[serde(default)]
    pub job_id: Option<String>,
    /// `owner/repo`, as the cloud knows it — the name the job was dispatched
    /// under, which is what a rebuilt record must carry.
    #[serde(default)]
    pub repo: Option<String>,
    /// The commit the prompts were built at.
    #[serde(default)]
    pub commit: Option<String>,
    /// RFC 3339, when the job was dispatched.
    #[serde(default)]
    pub created_at: Option<String>,
}

impl JobStatus {
    /// Whether there is something to collect. `partial` counts: every answer
    /// in it is one the resume does not have to ask for.
    pub fn is_ready(&self) -> bool {
        self.state == "ready" || self.state == "partial"
    }

    /// Whether nothing more is coming. `cancelled` is somebody's decision and
    /// `failed` is the driver's, and neither leaves anything to wait for.
    pub fn has_failed(&self) -> bool {
        self.state == "failed" || self.state == "cancelled"
    }

    /// How far through, as the cloud states it.
    pub fn percent(&self) -> Option<usize> {
        self.percent
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
    /// A reader for `carrick status`: bounded so a machine with no network
    /// costs the reader seconds.
    pub fn for_status() -> Result<Self, String> {
        Self::new(STATUS_TIMEOUT, Duration::from_secs(3))
    }

    /// A reader for `carrick resume`, which is collecting an object.
    pub fn for_collecting() -> Result<Self, String> {
        Self::new(DOWNLOAD_TIMEOUT, Duration::from_secs(10))
    }

    fn new(timeout: Duration, connect: Duration) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .connect_timeout(connect)
            .gzip(true)
            .build()
            .map_err(|_| "Could not reach Carrick Cloud")?;
        Ok(Self {
            client,
            endpoint: format!("{API_BASE}/types/check-or-upload"),
        })
    }

    /// One call, with a 404 answered as `Ok(None)`: a repo the cloud holds no
    /// job for is an answer, not a fault. Every other refusal is an error.
    async fn send(
        &self,
        credential: &Credential,
        body: serde_json::Value,
    ) -> Result<Option<serde_json::Value>, String> {
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&credential.token)
            .json(&body)
            .send()
            .await
            .map_err(|_| "Could not reach Carrick Cloud")?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
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
            .map(Some)
            .map_err(|_| "Carrick Cloud sent an answer this version cannot read".into())
    }

    /// The same call for a caller that has nothing to do with "no such job".
    async fn post(
        &self,
        credential: &Credential,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.send(credential, body)
            .await?
            .ok_or_else(|| "Carrick Cloud answered HTTP 404".to_string())
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

    /// The job the cloud holds for a repo, or `None` when it holds none.
    ///
    /// `analysis-job-status` takes `{ repo }` as well as `{ job_id }` and
    /// follows the repo -> job pointer the dispatch wrote, so this answers
    /// "is anything being analysed for this repo" for a machine that has no
    /// job id to ask with — a cleared `.carrick`, a second machine, a fresh
    /// clone. The body names the job, so the record can be rebuilt from it and
    /// the answers collected in the ordinary way (carrick#1320).
    pub async fn status_for_repo(
        &self,
        credential: &Credential,
        repo: &str,
    ) -> Result<Option<JobStatus>, String> {
        let Some(value) = self
            .send(
                credential,
                serde_json::json!({"action": "analysis-job-status", "repo": repo}),
            )
            .await?
        else {
            return Ok(None);
        };
        serde_json::from_value(value)
            .map(Some)
            .map_err(|_| "Carrick Cloud sent a job status this version cannot read".to_string())
    }

    /// Download the answers, and say what came with them.
    ///
    /// To a file rather than into memory: the answers are large, the scan
    /// subprocess is the thing that reads them, and a path is what crosses
    /// that boundary.
    ///
    /// `Ok(None)` is a job the cloud answered for and that holds nothing: no
    /// pass of it ever wrote an object. It is a value rather than an error
    /// because a caller deciding whether to keep the local record has to tell
    /// it apart from a machine that could not ask (carrick#1319).
    pub async fn answers(
        &self,
        credential: &Credential,
        job: &Job,
        into: &Path,
    ) -> Result<Option<Collected>, String> {
        let value = self
            .post(
                credential,
                serde_json::json!({"action": "analysis-job-answers", "job_id": job.job_id}),
            )
            .await?;
        let response: AnswersResponse = serde_json::from_value(value).map_err(|_| {
            "Carrick Cloud sent an answer list this version cannot read".to_string()
        })?;
        if response.schema != crate::analysis_job::ANSWERS_SCHEMA {
            return Err(format!(
                "Carrick Cloud answered with schema '{}'; this scanner reads {}. Update carrick \
                 to collect this analysis.",
                response.schema,
                crate::analysis_job::ANSWERS_SCHEMA
            ));
        }
        if response.parts.is_empty() {
            return Ok(None);
        }
        // One object per PASS, not one per job (carrick-cloud#1006): a pass
        // writes what it has before it hands on, so nothing anywhere holds the
        // whole job at once. They are folded into one file here, and the scan
        // that replays them is handed a path and knows nothing about how the
        // work was divided up.
        let _ = std::fs::create_dir_all(into);
        let path = into.join(format!("answers-{}.ndjson.gz", job.job_id));
        let mut writer = flate2::write::GzEncoder::new(
            std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?,
            flate2::Compression::default(),
        );
        for part in &response.parts {
            let text = self.download(&part.url).await?;
            std::io::Write::write_all(&mut writer, text.as_bytes())
                .and_then(|()| {
                    if text.ends_with('\n') {
                        Ok(())
                    } else {
                        std::io::Write::write_all(&mut writer, b"\n")
                    }
                })
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        writer
            .finish()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Some(Collected {
            answers: path,
            rows: response.parts.iter().map(|part| part.rows).sum(),
            superseded: response.superseded,
            superseded_by: response.current_index.and_then(|index| index.source),
        }))
    }

    /// GET one part, with the three conditions a link this machine follows
    /// without asking has to meet: https, no credentials of its own, and a
    /// host. The same three the hosted read applies to a staged URL.
    async fn download(&self, url: &str) -> Result<String, String> {
        let url = reqwest::Url::parse(url).map_err(|_| "Carrick Cloud sent an unusable link")?;
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
        if bytes.starts_with(&[0x1f, 0x8b]) {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(&bytes[..]), &mut text)
                .map_err(|_| "Could not read the analysis")?;
            return Ok(text);
        }
        String::from_utf8(bytes.to_vec()).map_err(|_| "Could not read the analysis".to_string())
    }
}

/// What a collected job amounts to, once its parts are on disk.
#[derive(Debug)]
pub struct Collected {
    /// Every part, folded into one file for the scan that replays them.
    pub answers: PathBuf,
    /// How many answers the cloud says it handed over.
    pub rows: usize,
    /// The stored index moved on while this job ran, so the index this resume
    /// writes is not the one the cloud should serve.
    ///
    /// **The cloud's answer, not a derived one.** A check-or-upload response
    /// says neither when a stored index landed nor what wrote it, and index
    /// rows carry no commit at all — so it is decided from the job's start
    /// time and the rows' source, on the side that holds both (R8, corrected
    /// on carrick-cloud#1006). Nothing on this side may claim a commit.
    pub superseded: bool,
    /// What wrote the index that moved past this one, when the cloud says.
    pub superseded_by: Option<String>,
}

/// The `analysis-job-answers` 200 body.
#[derive(Deserialize, Debug, Default)]
struct AnswersResponse {
    #[serde(default)]
    schema: String,
    #[serde(default)]
    parts: Vec<AnswerPart>,
    #[serde(default)]
    superseded: bool,
    #[serde(default)]
    current_index: Option<CurrentIndex>,
}

#[derive(Deserialize, Debug)]
struct AnswerPart {
    #[serde(default)]
    url: String,
    #[serde(default)]
    rows: usize,
}

#[derive(Deserialize, Debug)]
struct CurrentIndex {
    #[serde(default)]
    source: Option<String>,
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
        let reader = match JobReader::for_status() {
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

/// Ask the cloud which job it holds for each of these repos.
///
/// The other end of [`ask`], for a workspace that has no record to ask with.
/// `Ok(None)` is a repo nothing is being analysed for, which is the ordinary
/// answer and not a fault.
pub fn ask_by_repo(repos: &[String]) -> Vec<Result<Option<JobStatus>, String>> {
    let credential = match Credential::load() {
        Ok(Some(credential)) => credential,
        Ok(None) => {
            return repos.iter().map(|_| Err(SIGNED_OUT.to_string())).collect();
        }
        Err(error) => return repos.iter().map(|_| Err(error.clone())).collect(),
    };
    let asked: Vec<String> = repos.to_vec();
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => return asked.iter().map(|_| Err(UNREACHABLE.to_string())).collect(),
        };
        let reader = match JobReader::for_status() {
            Ok(reader) => reader,
            Err(error) => return asked.iter().map(|_| Err(error.clone())).collect(),
        };
        runtime.block_on(async {
            let mut answers = Vec::with_capacity(asked.len());
            for repo in &asked {
                answers.push(reader.status_for_repo(&credential, repo).await);
            }
            answers
        })
    })
    .join()
    .unwrap_or_else(|_| repos.iter().map(|_| Err(UNREACHABLE.to_string())).collect())
}

/// Download one job's answers, and say where they landed. `Ok(None)` is a job
/// that holds none.
pub fn download(job: &Job, into: &Path) -> Result<Option<Collected>, String> {
    let credential = Credential::load()?.ok_or(SIGNED_OUT)?;
    let job = job.clone();
    let into = into.to_path_buf();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| UNREACHABLE.to_string())?;
        let reader = JobReader::for_collecting()?;
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

    /// The file is read by something that is not this binary.
    ///
    /// `scripts/dispatch-smoke.sh` proves a dispatch happened by reading these
    /// keys with `jq`: the record is written only from a submission the cloud
    /// accepted and named, so it is the one artefact that tells a hand-off
    /// apart from a synchronous scan that exited 0. A rename here would leave
    /// that smoke passing over a run that dispatched nothing, which is the
    /// failure it exists to catch — so the names are pinned where a rename
    /// breaks the ordinary suite instead (carrick#1259).
    #[test]
    fn the_record_carries_the_names_the_dispatch_smoke_reads() {
        let dir = tempfile::tempdir().unwrap();
        record(dir.path(), job("owner/api", "j1")).unwrap();
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(jobs_file(dir.path())).unwrap()).unwrap();
        assert_eq!(written["schema"], JOBS_SCHEMA);
        let job = &written["jobs"][0];
        assert_eq!(job["job_id"], "j1");
        assert_eq!(job["repo"], "owner/api");
        assert_eq!(job["analyze_rows"], 10);
    }

    /// A `jobs.json` written by the PREVIOUS release still reads
    /// (carrick#1332).
    ///
    /// This is the file a customer's laptop is holding right now. It is the
    /// only thing that survives a dispatched scan, it outlives the scan record
    /// beside it on purpose, and `carrick resume` cannot find a job without
    /// it. A field renamed here reads as a `jobs.json` that was never written:
    /// [`read`] swallows every error and answers "no jobs", `carrick status`
    /// says nothing is in flight, and the answers it names are unreachable.
    ///
    /// Checked in verbatim as this release wrote it, so the next release has
    /// to read it or turn this red. Every field is asserted rather than
    /// "it parsed", because [`read`] returning an empty `Vec` is exactly what
    /// a silently broken parse looks like.
    #[test]
    fn a_jobs_file_the_previous_release_wrote_still_reads() {
        const PREVIOUS: &str = include_str!("../../tests/golden/jobs-0.3.81.json");

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(jobs_file(dir.path()), PREVIOUS).unwrap();
        let jobs = read(dir.path());

        assert_eq!(
            jobs.len(),
            1,
            "the record a dispatched scan left is unreadable to this version, so its answers \
             cannot be collected at all"
        );
        assert_eq!(
            jobs[0],
            Job {
                repo: "owner/api".to_string(),
                path: "/Users/someone/repos/api".to_string(),
                job_id: "job-0f3c1a9e".to_string(),
                commit: "9a3f2c1d4b5e6f7a8b9c0d1e2f3a4b5c6d7e8f90".to_string(),
                analyze_rows: 1017,
                submitted_at: "2026-09-18T09:14:22+00:00".to_string(),
            }
        );
        // And the tag it was written under is the one this version writes, so
        // the file a resume rewrites is the same file the next one reads.
        let written: serde_json::Value = serde_json::from_str(PREVIOUS).unwrap();
        assert_eq!(written["schema"], JOBS_SCHEMA);
    }

    #[test]
    fn an_unreadable_record_is_a_record_of_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(jobs_file(dir.path()), "{not json").unwrap();
        assert!(read(dir.path()).is_empty());
    }

    #[test]
    fn a_status_says_whether_there_is_anything_to_collect() {
        // The deployed body's own field names, and its own percentage.
        let running: JobStatus = serde_json::from_value(serde_json::json!({
            "state": "running", "answered": 8570, "total_rows": 13389, "percent": 64
        }))
        .unwrap();
        assert!(!running.is_ready());
        assert_eq!(running.percent(), Some(64));
        assert_eq!(running.answered, 8570);
        assert_eq!(running.total_rows, 13389);

        let ready: JobStatus =
            serde_json::from_value(serde_json::json!({"state": "ready"})).unwrap();
        assert!(ready.is_ready());
        assert_eq!(ready.percent(), None, "a fraction nobody sent is not shown");

        let partial: JobStatus =
            serde_json::from_value(serde_json::json!({"state": "partial"})).unwrap();
        assert!(partial.is_ready(), "what landed is still worth collecting");

        let cancelled: JobStatus =
            serde_json::from_value(serde_json::json!({"state": "cancelled"})).unwrap();
        assert!(cancelled.has_failed(), "nothing more is coming for it");

        let stopped: JobStatus = serde_json::from_value(
            serde_json::json!({"state": "failed", "failure_reason": "driver_stopped"}),
        )
        .unwrap();
        assert_eq!(stopped.failure_reason.as_deref(), Some("driver_stopped"));

        let unknown: JobStatus = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(!unknown.is_ready() && !unknown.has_failed());
    }

    /// The status body names the job, which is what makes a lost record
    /// recoverable: asked by repo, it answers with everything
    /// `.carrick/jobs.json` holds except the path, and the path is this
    /// machine's own (carrick#1320). The names are the deployed body's
    /// (`jobStatusBody`); a rename on either side loses the recovery path
    /// silently, so it fails here instead.
    #[test]
    fn a_status_names_the_job_a_lost_record_would_have_held() {
        let body: JobStatus = serde_json::from_value(serde_json::json!({
            "schema": "carrick.analysis-job/0", "job_id": "job_abc", "scan_id": null,
            "repo": "owner/api", "commit": "abc1234", "state": "ready",
            "total_rows": 1017, "answered": 1017,
            "created_at": "2026-09-17T21:00:00Z", "expires_at": "2026-10-01T21:00:00Z"
        }))
        .unwrap();
        assert_eq!(body.job_id.as_deref(), Some("job_abc"));
        assert_eq!(body.repo.as_deref(), Some("owner/api"));
        assert_eq!(body.commit.as_deref(), Some("abc1234"));
        assert_eq!(body.created_at.as_deref(), Some("2026-09-17T21:00:00Z"));
        assert_eq!(body.total_rows, 1017);
        // A body that carries none of them still parses: every field on this
        // wire is optional, and `carrick status` must answer whatever came.
        let bare: JobStatus =
            serde_json::from_value(serde_json::json!({"state": "running", "repo": null})).unwrap();
        assert_eq!(bare.job_id, None);
        assert_eq!(bare.repo, None);
    }

    #[test]
    fn how_long_ago_reads_as_a_person_would_say_it() {
        let ago = |seconds: i64| {
            since(&(chrono::Utc::now() - chrono::Duration::seconds(seconds)).to_rfc3339()).unwrap()
        };
        assert_eq!(ago(30), "1 minute");
        assert_eq!(ago(600), "10 minutes");
        assert_eq!(ago(7200), "2 hours");
        assert_eq!(ago(518_400), "6 days");
        assert_eq!(
            until(&(chrono::Utc::now() - chrono::Duration::days(1)).to_rfc3339()),
            None
        );
    }
}
