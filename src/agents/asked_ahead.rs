//! The file reader's answers for a service, asked before its analysis asks
//! (carrick#3131).
//!
//! A lane in the engine builds a service's prompts while the scan analyses
//! the service before it, and asks the reader for them. It holds each file's
//! answer here under the bytes it asked. When the service's own analysis
//! reaches its asks, it looks each file up under the bytes it built: the
//! same bytes take the answer held, still in flight or not, and any other
//! bytes are asked as they always were. So nothing is asked twice and no
//! answer is read for a prompt that was not sent.
//!
//! The service's analysis and the lane meet at one place, [`AheadSlot`]: the
//! lane publishes what it holds there, and the analysis claims it once. A
//! slot claimed before the lane published is closed, and the lane then asks
//! nothing for the service.

use crate::agents::file_analyzer_agent::FileAnalysisResult;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// What an answer is held under: the file's prompt path, and the digest of
/// every byte its first request carries, or of every part's when it is asked
/// in parts. The guidance prefix is in the digest: a prompt is the same
/// prompt only when the guidance it embeds is too.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AheadKey {
    pub prompt_path: String,
    pub prompt_id: String,
}

/// One file's answer as it is held: `None` once it arrives is an ask that
/// failed, which the analysis asks again.
struct Held {
    answer: tokio::sync::oneshot::Receiver<Option<FileAnalysisResult>>,
    /// Set once the lane has sent the request.
    asked: Arc<AtomicBool>,
}

/// The answers asked ahead for one service.
#[derive(Default)]
pub struct AskedAhead {
    held: Mutex<HashMap<AheadKey, Held>>,
    asked: AtomicUsize,
    taken: AtomicUsize,
    /// Whether the analysis has said it waits for an answer in flight.
    said_waiting: AtomicBool,
}

/// The lane's end of one held answer.
pub struct Hold {
    answer: tokio::sync::oneshot::Sender<Option<FileAnalysisResult>>,
    asked: Arc<AtomicBool>,
}

impl Hold {
    /// The lane is sending the request.
    pub fn asking(&self, ahead: &AskedAhead) {
        if !self.asked.swap(true, Ordering::SeqCst) {
            ahead.asked.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// The answer, or `None` for an ask that failed. Dropping a hold
    /// unanswered is the same as `None`.
    pub fn answer(self, answer: Option<FileAnalysisResult>) {
        let _ = self.answer.send(answer);
    }
}

impl AskedAhead {
    /// Hold a place for the answer under `key`. `None` when one is held there
    /// already: the second file is left to the analysis.
    pub fn hold(&self, key: AheadKey) -> Option<Hold> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        if held.contains_key(&key) {
            return None;
        }
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let asked = Arc::new(AtomicBool::new(false));
        held.insert(
            key,
            Held {
                answer: receiver,
                asked: asked.clone(),
            },
        );
        Some(Hold {
            answer: sender,
            asked,
        })
    }

    /// The answer held under `key`, once it has arrived. `None` when none is
    /// held there or the lane's ask failed: the analysis asks for itself.
    /// Each key is taken once.
    ///
    /// The first time the analysis has to wait for an answer still in
    /// flight, it says so once on the run's log: a long wait here is a stall
    /// a reader of the log can then place (carrick#748).
    pub async fn take(&self, key: &AheadKey) -> Option<FileAnalysisResult> {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(key)?;
        let answer = match held.answer.try_recv() {
            Ok(answer) => answer,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => None,
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                if !self.said_waiting.swap(true, Ordering::SeqCst) {
                    tracing::info!(
                        "Waiting for the file reader's answers asked ahead for {}",
                        crate::current_service::name()
                            .as_deref()
                            .unwrap_or("(root)")
                    );
                }
                held.answer.await.ok().flatten()
            }
        }?;
        self.taken.fetch_add(1, Ordering::SeqCst);
        Some(answer)
    }

    /// Requests the lane sent.
    pub fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }

    /// Answers the analysis took.
    pub fn taken(&self) -> usize {
        self.taken.load(Ordering::SeqCst)
    }

    /// Requests the lane sent whose answer the analysis never looked up: it
    /// built those files' prompts in other bytes, so they were spend with no
    /// saving.
    pub fn unused(&self) -> usize {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter(|held| held.asked.load(Ordering::SeqCst))
            .count()
    }

    /// The run log's line for `service`, once its analysis has asked.
    pub fn line(&self, service: &str) -> String {
        format!(
            "Answers ahead for {service}: asked {} file(s), taken {}, unused {}",
            self.asked(),
            self.taken(),
            self.unused()
        )
    }
}

/// Where a service's analysis and the lane asking ahead for it meet.
#[derive(Default)]
pub struct AheadSlot(Mutex<SlotState>);

#[derive(Default)]
enum SlotState {
    /// Neither has come.
    #[default]
    Open,
    /// The lane published what it holds.
    Published(Arc<AskedAhead>),
    /// The analysis came first, or took what was published.
    Closed,
}

impl AheadSlot {
    /// Publish what the lane holds. `false` when the analysis has already
    /// claimed the slot: the lane must then ask nothing.
    pub fn publish(&self, ahead: Arc<AskedAhead>) -> bool {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        match *state {
            SlotState::Open => {
                *state = SlotState::Published(ahead);
                true
            }
            _ => false,
        }
    }

    /// What the lane published, taken once, by the analysis. Closes the slot
    /// either way, so a lane that has not published yet asks nothing.
    pub fn claim(&self) -> Option<Arc<AskedAhead>> {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        match std::mem::replace(&mut *state, SlotState::Closed) {
            SlotState::Published(ahead) => Some(ahead),
            _ => None,
        }
    }

    /// What the lane published, while the analysis has not claimed it, read
    /// without claiming it: for a reader of its counts.
    pub fn held(&self) -> Option<Arc<AskedAhead>> {
        match &*self.0.lock().unwrap_or_else(PoisonError::into_inner) {
            SlotState::Published(ahead) => Some(ahead.clone()),
            _ => None,
        }
    }

    /// Whether the analysis has claimed the slot.
    pub fn claimed(&self) -> bool {
        matches!(
            *self.0.lock().unwrap_or_else(PoisonError::into_inner),
            SlotState::Closed
        )
    }
}

/// Which side of the slot a [`super::file_orchestrator::FileOrchestrator`]
/// works for.
#[derive(Clone, Default)]
pub enum AheadRole {
    /// Neither: every file is asked as it always was.
    #[default]
    None,
    /// The service's analysis: it claims the slot before it asks.
    Analysis(Arc<AheadSlot>),
    /// The lane: it builds the prompts, publishes, asks, and stops there.
    /// `stop` is set when nothing more is wanted for this service: its
    /// analysis refused the lane, or the scan stopped it. The lane builds
    /// quietly, and says on `log` what it asks and what its asks say.
    Lane {
        slot: Arc<AheadSlot>,
        stop: Arc<AtomicBool>,
        log: tracing::Dispatch,
    },
}

impl AheadRole {
    pub fn is_lane(&self) -> bool {
        matches!(self, Self::Lane { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(path: &str) -> AheadKey {
        AheadKey {
            prompt_path: path.to_string(),
            prompt_id: "id".to_string(),
        }
    }

    #[tokio::test]
    async fn an_answer_is_taken_once_and_counted() {
        let ahead = AskedAhead::default();
        let hold = ahead.hold(key("a.ts")).expect("a place");
        assert!(ahead.hold(key("a.ts")).is_none(), "one place per key");
        hold.asking(&ahead);
        hold.answer(Some(FileAnalysisResult::default()));
        assert!(ahead.take(&key("a.ts")).await.is_some());
        assert!(ahead.take(&key("a.ts")).await.is_none(), "taken once");
        assert_eq!((ahead.asked(), ahead.taken(), ahead.unused()), (1, 1, 0));
    }

    #[tokio::test]
    async fn a_failed_or_dropped_ask_is_a_miss_and_not_unused() {
        let ahead = AskedAhead::default();
        let failed = ahead.hold(key("a.ts")).unwrap();
        failed.asking(&ahead);
        failed.answer(None);
        drop(ahead.hold(key("b.ts")).unwrap());
        assert!(ahead.take(&key("a.ts")).await.is_none());
        assert!(ahead.take(&key("b.ts")).await.is_none());
        assert_eq!((ahead.asked(), ahead.taken(), ahead.unused()), (1, 0, 0));
    }

    #[test]
    fn an_asked_answer_never_looked_up_is_unused() {
        let ahead = AskedAhead::default();
        let hold = ahead.hold(key("a.ts")).unwrap();
        hold.asking(&ahead);
        assert_eq!(ahead.unused(), 1);
        assert_eq!(
            ahead.line("web"),
            "Answers ahead for web: asked 1 file(s), taken 0, unused 1"
        );
    }

    #[test]
    fn a_slot_claimed_first_refuses_the_lane() {
        let slot = AheadSlot::default();
        assert!(slot.claim().is_none());
        assert!(slot.claimed());
        assert!(!slot.publish(Arc::new(AskedAhead::default())));
        let slot = AheadSlot::default();
        assert!(slot.publish(Arc::new(AskedAhead::default())));
        assert!(slot.claim().is_some());
        assert!(slot.claim().is_none(), "claimed once");
    }
}
