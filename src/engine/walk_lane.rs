//! Every service's compiler walk, run before the analysis loop
//! (carrick#2413 slice G1).
//!
//! With `CARRICK_POSITION_ROWS=1` each service's per-position rows are
//! walked in a record-only sidecar of their own (carrick#2405). The walks
//! used to run inside the loop, one before each service's analysis. They
//! now run in a lane of their own as the scan starts, in declared order and
//! as many at once as the loop analyses, beside the queue that asks each
//! service's setup ahead ([`super::setup_ahead`]):
//!
//! * **Each setup waits for its own walk, and no other.** A service's plan is
//!   read once its walk has ended ([`WalkEnded`]), so the first service's
//!   setup is answered while the other walks run. Asking a setup from inside
//!   the loop instead, after that service's walk, undoes carrick#1895: the
//!   loop would wait for every setup again.
//! * **The loop starts once the lane has ended.** No walk process is alive
//!   beside an analysis sidecar, so the two programs never share the
//!   machine's memory.
//! * **The rows are kept until the run ends** ([`Walked`]): read by the
//!   service's analysis and again by the owed-work retry, from one walk.
//!
//! Off, there is no lane: the queue and the loop run as they did.

use std::path::PathBuf;

use tokio::sync::oneshot;
use tracing::debug;

use super::service_workers;
use crate::config::Config;
use crate::position_rows::{Destination, PositionIndex};
use crate::services::type_sidecar::SidecarRecipe;

/// Heard by a service's setup once its walk has ended, however it ended:
/// a walk that failed or panicked drops its sender, which is heard the same.
pub(super) type WalkEnded = oneshot::Receiver<()>;

/// One service's walk, as the run keeps it until it ends.
#[derive(Debug)]
pub(super) struct Walked {
    /// The rows file, when the walk wrote one. A temporary one is removed
    /// when this is dropped, which is when the run ends.
    rows: Option<Destination>,
    scan_root: PathBuf,
    label: String,
}

impl Walked {
    /// The service's rows for one pass of its analysis, read from the file
    /// its walk wrote. A walk that wrote none gives an index with no rows,
    /// every lookup in which is a hole.
    pub(super) fn index(&self) -> PositionIndex {
        match &self.rows {
            Some(rows) => crate::position_rows::load_index(rows, &self.scan_root, &self.label),
            None => PositionIndex::missing(&self.scan_root),
        }
    }
}

/// Whether this run walks the rows at all.
pub(super) fn requested() -> bool {
    std::env::var(crate::position_rows::POSITION_ROWS_ENV).as_deref() == Ok("1")
}

/// One heard [`WalkEnded`] per service, and the senders the lane drops as
/// each walk ends.
pub(super) fn signals(count: usize) -> (Vec<oneshot::Sender<()>>, Vec<WalkEnded>) {
    (0..count).map(|_| oneshot::channel()).unzip()
}

/// Walk `services` in declared order, `width` at a time, each on a thread of
/// its own, and return each one's [`Walked`] at its position. Each service's
/// sender in `ended` is dropped as its walk ends. `at` is, per service, the
/// commit whose inputs its rows are a walk of, or `None` when no commit
/// vouches for them (a dirty tree); it is recorded beside the rows so a later
/// pass that carries the service at that commit can use them (carrick#3027).
pub(super) async fn walk_all(
    recipe: Option<SidecarRecipe>,
    repo_path: &str,
    services: &[Config],
    width: usize,
    ended: Vec<oneshot::Sender<()>>,
    mut at: Vec<Option<String>>,
) -> Vec<Walked> {
    let mut ended: Vec<Option<oneshot::Sender<()>>> = ended.into_iter().map(Some).collect();
    ended.resize_with(services.len(), || None);
    at.resize_with(services.len(), || None);
    service_workers::admit_in_declared_order(services.len(), width, |position| {
        let (recipe, repo_path) = (recipe.clone(), repo_path.to_string());
        let service = services[position].clone();
        let ended = ended[position].take();
        let at = at[position].take();
        service_workers::on_a_thread(format!("carrick-walk-{}", position + 1), move |_| {
            // Dropped when the walk ends, whichever way: the setup waiting
            // for it hears that and is planned.
            let _ended = ended;
            let label = service.service_name.as_deref().unwrap_or("(root)");
            let span = tracing::info_span!("service", name = %label);
            let _entered = span.enter();
            walk(recipe.as_ref(), &repo_path, &service, at.as_deref())
        })
    })
    .await
}

/// The files a walk of `service` sends, sorted, each under `scan_root`.
fn service_files(repo_path: &str, service: &Config, scan_root: &std::path::Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> =
        super::walk_service(repo_path, service, &super::service_ignore_patterns(service))
            .files
            .iter()
            .map(|file| scan_root.join(file.strip_prefix(repo_path).unwrap_or(file)))
            .collect();
    files.sort();
    files
}

/// The rows an earlier pass kept for `service`, carried at `commit`, as a
/// [`Walked`], when they are exactly what a walk would write now
/// ([`crate::position_rows::kept_rows`]); otherwise why not. Nothing is
/// spawned: a service this returns for is not walked (carrick#3027).
pub(super) fn reuse(
    repo_path: &str,
    service: &Config,
    commit: &str,
) -> Result<Walked, crate::position_rows::Miss> {
    let canonical = super::canonical_repo_path(repo_path);
    let scan_root = PathBuf::from(&canonical);
    let label = service
        .service_name
        .as_deref()
        .unwrap_or("(root)")
        .to_string();
    let kept = crate::position_rows::kept_path(
        &super::get_repository_name(&canonical),
        service.service_name.as_deref(),
    )
    .ok_or(crate::position_rows::Miss::NotKept)?;
    let files = service_files(repo_path, service, &scan_root);
    crate::position_rows::kept_rows(&kept, commit, &scan_root, &files)?;
    Ok(Walked {
        rows: Some(Destination::kept(kept)),
        scan_root,
        label,
    })
}

/// Walk one service's per-position compiler rows: written beside its blob
/// with a local store, under the OS temp dir otherwise, and named by the
/// stem the blob will carry. The files are the ones discovery walks; every
/// path is sent under the repo's canonical root, as the service's sidecar
/// root is.
///
/// A run with no sidecar, or a walk that failed, keeps no rows. A walk that
/// fails says so in the run log, and nothing else.
fn walk(
    recipe: Option<&SidecarRecipe>,
    repo_path: &str,
    service: &Config,
    at: Option<&str>,
) -> Walked {
    let canonical = super::canonical_repo_path(repo_path);
    let scan_root = PathBuf::from(&canonical);
    let label = service
        .service_name
        .as_deref()
        .unwrap_or("(root)")
        .to_string();
    let no_rows = |scan_root: PathBuf, label: String| Walked {
        rows: None,
        scan_root,
        label,
    };
    let Some(out) = crate::position_rows::destination(
        &super::get_repository_name(&canonical),
        service.service_name.as_deref(),
    ) else {
        return no_rows(scan_root, label);
    };
    let Some(recipe) = recipe else {
        return no_rows(scan_root, label);
    };
    let files = service_files(repo_path, service, &scan_root);
    let service_root = super::service_sidecar_root(repo_path, service);
    let walk = crate::position_rows::ServiceWalk {
        service_root: &service_root,
        tsconfig: service.tsconfig.as_deref(),
        scan_root: &scan_root,
        files: &files,
        out: out.path(),
        service: &label,
    };
    if let Err(error) = crate::position_rows::record_service_rows(recipe, &walk) {
        debug!("Position rows for service {label} were not written: {error}");
        return no_rows(scan_root, label);
    }
    crate::position_rows::record_walked_at(out.path(), at);
    Walked {
        rows: Some(out),
        scan_root,
        label,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With no sidecar to walk in there are no rows, and every lookup is a
    /// hole.
    #[test]
    fn a_walk_without_a_sidecar_keeps_no_rows() {
        let repo = tempfile::tempdir().unwrap();
        let walked = walk(
            None,
            &repo.path().to_string_lossy(),
            &Config::default(),
            None,
        );
        assert!(walked.rows.is_none());
        assert_eq!(walked.index().file_count(), 0);
    }
}
