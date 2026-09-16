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

use std::sync::{Mutex, MutexGuard, PoisonError};

/// The service whose analysis is running, or `None` outside the loop.
static CURRENT: Mutex<Option<String>> = Mutex::new(None);

fn current_slot() -> MutexGuard<'static, Option<String>> {
    // A panic inside one service's analysis leaves the lock poisoned; the
    // value behind it is a name, and reading a stale name is better than
    // taking down every later call with a second panic.
    CURRENT.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The name of the service being analysed, or `None` when no analysis is in
/// scope (the cross-repo phase, the upload) or when the service in scope is
/// unnamed.
pub fn name() -> Option<String> {
    current_slot().clone()
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
    ServiceScope { previous }
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
        {
            let _scope = enter(Some("api"));
            assert_eq!(name().as_deref(), Some("api"));
        }
        assert_eq!(name(), None, "the gap between two services names neither");
        {
            let _scope = enter(Some("web"));
            assert_eq!(name().as_deref(), Some("web"));
        }
        assert_eq!(name(), None, "the cross-repo phase names no service");
    }

    /// An unnamed service — the single-service case — is in scope but has no
    /// name to report, so it sends no header rather than the `(root)` the log
    /// lines use.
    #[test]
    #[serial_test::serial(current_service)]
    fn an_unnamed_service_reports_no_name() {
        let _scope = enter(None);
        assert_eq!(name(), None);
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
