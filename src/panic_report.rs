//! What a panicking scan says before it goes (carrick#1470).
//!
//! Every other way a run can end now reports itself: a failure marks the scan
//! and ships the log ([`crate::engine`]), a signal ends the run through that
//! same path ([`crate::shutdown`]), and a print to a terminal that went away
//! no longer takes the process with it ([`crate::console`]). A panic did not.
//! It unwound, the process left with 101, and nothing was said: the cloud held
//! the run's slot until its TTL, the run log stopped wherever the writer had
//! got to, and the panic text went to a terminal nobody was reading.
//!
//! So a hook says it first, in the two places it can be read afterwards: this
//! run's own log file, and `scan-failed` with `panic` as the reason. Both are
//! best effort on a process that is already going, and the whole of it is
//! bounded by [`INTERRUPTION_REPORT_BUDGET`], the same budget the signal path
//! spends on the same question. Then the unwind carries on exactly as it did.
//!
//! What this cannot cover: an abort. A stack overflow raises SIGSEGV and the
//! runtime aborts on it, and `panic = "abort"` would skip every hook; neither
//! runs this. The cloud's slot TTL is what covers those, as it covers a
//! SIGKILL. Nor a panic raised while this thread already holds the log file's
//! own lock, where the line below would wait on itself: the file layer answers
//! a write error rather than panicking on one (`log_internal_errors(false)`,
//! carrick#1386), so there is no such panic to raise today.

use crate::cloud_storage::AwsStorage;
use crate::shutdown::INTERRUPTION_REPORT_BUDGET;
use std::panic::PanicHookInfo;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{debug, error};

/// The word a panicking run reports, the way [`crate::shutdown`] has one.
///
/// It leads the reason the cloud stores, so a dashboard reading `reason`
/// answers "what killed it" from the first word and "where" from the rest.
pub const PANIC_REASON: &str = "panic";

/// Whether this process has already reported a panic.
///
/// One panic can reach the hook twice: a panicking intents task is joined and
/// re-raised on the main thread ([`crate::intent_generator`]), which is the
/// same death said twice. The marker is posted for the first, and the default
/// hook still runs for both, so nothing a reader sees changes.
static REPORTED: AtomicBool = AtomicBool::new(false);

/// Report any panic from here on, then let it unwind as it would have.
///
/// Installed once, as early in a scan as there is a log to write to. The
/// hook it replaces is kept and called last: the message a user reads is the
/// one Rust writes, in the place and the wording they expect it.
pub fn install(repo_path: String) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        report(info, &repo_path);
        previous(info);
    }));
}

fn report(info: &PanicHookInfo<'_>, repo_path: &str) {
    if REPORTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let sentence = panic_sentence(info);
    // The log first: it is a write to a file this process already holds open,
    // and it is what anyone diagnosing the run reads.
    error!("{sentence}");
    let redaction = crate::logging::Redaction::for_run(Some(repo_path));
    post_marker(crate::engine::fail_reason(&sentence, &redaction));
}

/// The panic as one line: what it was raised with, and where.
///
/// A payload is a `&str` or a `String` for every panic Rust itself raises and
/// for every `panic!` written in this crate. Anything else is reported without
/// its text rather than not reported.
fn panic_sentence(info: &PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    let said = payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "no message".to_string());
    match info.location() {
        Some(at) => format!(
            "{PANIC_REASON} at {}:{}:{}: {said}",
            at.file(),
            at.line(),
            at.column()
        ),
        None => format!("{PANIC_REASON}: {said}"),
    }
}

/// Mark the scan failed, inside the budget, from a thread of this hook's own.
///
/// A thread because the hook has no runtime it may use: it runs wherever the
/// panic was raised, which on this process is usually one of the runtime's own
/// worker threads, and blocking one of those on a request is a deadlock. A
/// current-thread runtime built here owes nothing to the one that is unwinding.
///
/// Bounded twice, for two different hangs: the timeout inside covers the
/// request, and the wait out here covers everything before it, including a
/// runtime that will not build. Whichever expires, the hook returns and the
/// unwind carries on; the thread goes with the process.
fn post_marker(reason: String) {
    let Some(scan_id) = crate::credentials::scan_id() else {
        debug!("No panic marker: this run has no open scan to mark.");
        return;
    };
    let (done, finished) = std::sync::mpsc::sync_channel::<()>(1);
    let spawned = std::thread::Builder::new()
        .name("carrick-panic-report".to_string())
        .spawn(move || {
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(async {
                    let _ = tokio::time::timeout(INTERRUPTION_REPORT_BUDGET, async {
                        match AwsStorage::new(false) {
                            Ok(storage) => report_panic(&storage, scan_id, &reason).await,
                            Err(e) => debug!("No panic marker: {e}"),
                        }
                    })
                    .await;
                }),
                Err(e) => debug!("No panic marker: no runtime to send it with: {e}"),
            }
            let _ = done.send(());
        });
    if let Err(e) = spawned {
        debug!("No panic marker: no thread to send it from: {e}");
        return;
    }
    let _ = finished.recv_timeout(INTERRUPTION_REPORT_BUDGET);
}

/// `scan-failed` for the scan this process opened, naming the panic.
///
/// The slot comes from the process-global rather than from the storage: this
/// storage was built here, in the hook, and has opened nothing of its own.
async fn report_panic(storage: &AwsStorage, scan_id: &str, reason: &str) {
    storage
        .report_scan_failed_for(scan_id, crate::scan_stage::current().as_str(), reason)
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_storage::AwsStorage;
    use crate::credentials::CloudAuth;

    /// The JSON one stubbed request carried.
    fn body_of(request: &str) -> serde_json::Value {
        let body = request.split("\r\n\r\n").nth(1).unwrap_or_default();
        serde_json::from_str(body).unwrap_or_else(|e| panic!("body {body:?}: {e}"))
    }

    /// The marker a panicking run sends: the scan it opened, the stage it died
    /// in, and a reason that leads with the word and carries the message and
    /// the place (carrick#1470).
    ///
    /// Nothing here installs the hook. A panic hook is process-global, and one
    /// set by a test in this binary would report for the two thousand other
    /// tests that share the process; what the hook does with a panic is proven
    /// in `tests/panic_hook_test.rs`, one test in a process of its own.
    #[tokio::test]
    async fn a_panicking_run_marks_its_scan_failed_and_names_the_panic() {
        let (base, server) = crate::agent_service::tests::stub_server(vec![(
            200,
            serde_json::json!({ "ok": true }).to_string(),
        )]);
        let storage = AwsStorage::for_test(
            &format!("{base}/types/check-or-upload"),
            CloudAuth::Bearer("carrick_sk_live_test".to_string()),
            false,
        );

        report_panic(
            &storage,
            "scan_01J",
            "panic at src/engine/mod.rs:12:5: a message",
        )
        .await;

        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 1, "{requests:?}");
        let body = body_of(&requests[0]);
        assert_eq!(body["action"], "scan-failed");
        assert_eq!(body["scan_id"], "scan_01J");
        assert_eq!(body["reason"], "panic at src/engine/mod.rs:12:5: a message");
        assert!(body.get("stage").is_some(), "{body}");
    }
}
