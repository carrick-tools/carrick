//! Which service's analysis is running right now, for the prompt-lambda
//! header builder.
//!
//! A scan of a monorepo analyses one service at a time (the loop in
//! [`crate::engine`]), and every prompt-lambda call a service makes is billed
//! to the repo as a whole. Nothing on the request said which tree spent the
//! money, so attributing a monorepo's spend meant counting `framework-detect`
//! calls to find the service boundaries and segmenting every other request by
//! the gaps between them. [`name`] is what `X-Carrick-Service` carries so
//! that is a log filter instead (carrick#1221, carrick-cloud#978).
//!
//! A process-global for the same reason [`crate::credentials::scan_id`] is:
//! the four prompt-lambda clients are built far from the loop, with no handle
//! on it, and the intent generator's work runs on spawned tasks — so a
//! thread-local would be read as empty by exactly the calls that cost the
//! most. The loop is sequential, so one value at a time is the whole truth.
//!
//! The contract is **present while that service's analysis runs and absent
//! otherwise**: the cross-repo phase and the upload belong to no service, and
//! a service that fails mid-loop must not leak its name into what follows.
//! That is why [`enter`] hands back a guard rather than setting a value:
//! the scope ends when the guard drops, including on the `?` that carries an
//! error out of the middle of the analysis.
//!
//! One kind of call is made for a service the loop has not reached: its
//! detection and guidance, asked ahead of its analysis (carrick#1895). Those
//! run on a task of their own inside [`asked_for`], which names the service
//! for every call that task makes, whatever the loop is analysing meanwhile.

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// The service whose analysis is running, or `None` outside the loop.
static CURRENT: Mutex<Option<String>> = Mutex::new(None);

/// How many scopes [`enter`] has open: above zero exactly while a service's
/// analysis runs, named or not, which [`CURRENT`] cannot say for an unnamed
/// service.
static OPEN_SCOPES: AtomicUsize = AtomicUsize::new(0);

fn current_slot() -> MutexGuard<'static, Option<String>> {
    // A panic inside one service's analysis leaves the lock poisoned; the
    // value behind it is a name, and reading a stale name is better than
    // taking down every later call with a second panic.
    CURRENT.lock().unwrap_or_else(PoisonError::into_inner)
}

tokio::task_local! {
    /// The service the running task is asking for, when that is not the one
    /// the loop is analysing. See [`asked_for`].
    static ASKED_FOR: Option<String>;
}

/// The name of the service a call is made for: the one the running task was
/// started for ([`asked_for`]), otherwise the one being analysed. `None` when
/// no analysis is in scope (the cross-repo phase, the upload) or when the
/// service is unnamed.
pub fn name() -> Option<String> {
    match ASKED_FOR.try_with(Clone::clone) {
        Ok(asked_for) => asked_for,
        Err(_) => current_slot().clone(),
    }
}

/// Whether the running task is asking for a service ahead of its analysis
/// ([`asked_for`]), and for which. The outer `None` is "no": the call belongs
/// to whatever the loop is analysing.
pub fn asked_ahead() -> Option<Option<String>> {
    ASKED_FOR.try_with(Clone::clone).ok()
}

/// Run `work` as `service`'s: every call it makes names `service`, and none
/// names the service the loop is analysing while it runs. `None` is an
/// unnamed service, which names nobody, as in [`enter`].
///
/// The name follows the task, not the process, so it covers what `work`
/// awaits and nothing it spawns.
pub async fn asked_for<Work: Future>(service: Option<String>, work: Work) -> Work::Output {
    ASKED_FOR.scope(service, work).await
}

/// Marks `service` as the one being analysed until the returned guard drops.
///
/// `None` — a single unnamed service — enters a scope that reports no name,
/// which is the same fact on the cloud side as no header at all.
#[must_use = "the scope ends when the guard drops, so binding it to `_` sets \
              nothing; bind it to a named local"]
pub fn enter(service: Option<&str>) -> ServiceScope {
    let mut slot = current_slot();
    let previous = slot.take();
    *slot = service.map(str::to_string);
    OPEN_SCOPES.fetch_add(1, Ordering::SeqCst);
    ServiceScope { previous }
}

/// Whether a service's analysis is running, named or unnamed. `false` in the
/// cross-repo phase and the upload, which belong to no service.
pub fn in_scope() -> bool {
    OPEN_SCOPES.load(Ordering::SeqCst) > 0
}

/// Holds one service's analysis open. See [`enter`].
pub struct ServiceScope {
    /// Restored on drop. Always `None` in the scan loop, which enters one
    /// scope per service and never nests; kept so that a nested scope could
    /// not silently end its parent's.
    previous: Option<String>,
}

impl Drop for ServiceScope {
    fn drop(&mut self) {
        *current_slot() = self.previous.take();
        OPEN_SCOPES.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Every test that reads or writes the value — here and in
/// [`crate::agent_service`], which asserts on the header it produces — runs
/// under `#[serial(current_service)]`. They share one process, so a test
/// asserting that a call outside the loop carries no service would otherwise
/// see the name a test running beside it had just entered.
#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing is in scope before the loop starts or after it ends: the
    /// cross-repo phase and the upload belong to no service.
    #[test]
    #[serial_test::serial(current_service)]
    fn nothing_is_in_scope_by_default() {
        assert_eq!(name(), None);
    }

    /// The loop's shape, two services deep: each one's name is current for
    /// its own analysis and for nothing either side of it.
    #[test]
    #[serial_test::serial(current_service)]
    fn each_service_is_current_only_for_its_own_analysis() {
        assert!(!in_scope());
        {
            let _scope = enter(Some("api"));
            assert_eq!(name().as_deref(), Some("api"));
            assert!(in_scope());
        }
        assert_eq!(name(), None, "the gap between two services names neither");
        assert!(!in_scope());
        {
            let _scope = enter(Some("web"));
            assert_eq!(name().as_deref(), Some("web"));
        }
        assert_eq!(name(), None, "the cross-repo phase names no service");
        assert!(!in_scope(), "the cross-repo phase is no service's analysis");
    }

    /// A service's detection and guidance are asked while the loop analyses
    /// another service (carrick#1895). Each call names the service it is for,
    /// on its own task, and the loop's name is untouched beside it.
    #[tokio::test]
    #[serial_test::serial(current_service)]
    async fn a_task_asking_ahead_names_its_own_service_and_not_the_loops() {
        let _scope = enter(Some("api"));
        let ahead = tokio::spawn(asked_for(Some("web".to_string()), async {
            tokio::task::yield_now().await;
            (name(), asked_ahead())
        }));
        let unnamed = tokio::spawn(asked_for(None, async { (name(), asked_ahead()) }));

        assert_eq!(
            ahead.await.unwrap(),
            (Some("web".to_string()), Some(Some("web".to_string())))
        );
        assert_eq!(
            unnamed.await.unwrap(),
            (None, Some(None)),
            "an unnamed service asked ahead names nobody, not the loop's service"
        );
        assert_eq!(name().as_deref(), Some("api"));
        assert_eq!(asked_ahead(), None, "the loop itself asks ahead for nobody");
    }

    /// An unnamed service — the single-service case — is in scope but has no
    /// name to report, so it sends no header rather than the `(root)` the log
    /// lines use.
    #[test]
    #[serial_test::serial(current_service)]
    fn an_unnamed_service_reports_no_name() {
        let _scope = enter(None);
        assert_eq!(name(), None);
        assert!(in_scope(), "its analysis is running all the same");
    }

    /// A service whose analysis fails carries its error out through `?`,
    /// which drops the guard on the way. The service after it, and the
    /// cross-repo phase, must not be billed the failed one's name.
    #[test]
    #[serial_test::serial(current_service)]
    fn an_error_mid_analysis_leaves_nothing_in_scope() {
        fn analyse_and_fail() -> Result<(), String> {
            let _scope = enter(Some("api"));
            assert_eq!(name().as_deref(), Some("api"));
            Err("manifest unreadable".to_string())?;
            unreachable!("the `?` above leaves the function")
        }

        assert!(analyse_and_fail().is_err());
        assert_eq!(name(), None);
    }
}
