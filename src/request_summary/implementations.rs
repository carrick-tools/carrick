//! The implementations of an interface member a call goes through
//! (carrick#2432 slice B), asked of the analysis sidecar and read by
//! discovery where its walk would stop at `interface_member`.
//!
//! The question is asked once per service, in one batch, for every call
//! whose declaration is a bodyless member of an interface or type the
//! service writes ([`RequestSummaryInputs::implementation_queries`]). The
//! sidecar answers with go-to-implementation over the service's program
//! ([`crate::services::TypeSidecar::implementations`]). Discovery reads the
//! answer through [`super::discovery::Labels::following`]: each
//! implementation is entered as the definition it is, and the walk's
//! agreement rule applies (one shared labelled end is a site, disagreement is
//! a lead, `several_implementations`; more than the sidecar's cap is
//! `implementations_over_cap`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::Stop;
use crate::position_rows::Kind;
use crate::services::type_sidecar::{ImplementationsAnswer, ImplementationsQuery};

use super::RequestSummaryInputs;
use super::row_calls::every_call;

/// What the sidecar found for one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Followed {
    /// The implementations, by file then position, as rows name them.
    Found(Vec<crate::services::type_sidecar::NamedDeclaration>),
    /// More than the sidecar lists: it names none.
    OverCap,
}

/// The sidecar's answers for a service's calls through interface members,
/// by the call's file as walked and SWC span (discovery's numbering). Empty
/// when nothing was asked or the sidecar did not answer: discovery then
/// stops at `interface_member` as it always has.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Implementations {
    by_call: HashMap<(PathBuf, u32, u32), Followed>,
    /// How many calls were asked about, for the run log.
    pub asked: usize,
}

impl Implementations {
    /// The answers `answers` give the calls `asked`, in the order asked. A
    /// question the sidecar did not search (`reason`) or that found nothing
    /// holds no entry.
    pub(super) fn of(asked: Vec<(PathBuf, u32, u32)>, answers: Vec<ImplementationsAnswer>) -> Self {
        let count = asked.len();
        let by_call = asked
            .into_iter()
            .zip(answers)
            .filter_map(|(call, answer)| {
                let followed = match answer {
                    ImplementationsAnswer { too_many: true, .. } => Followed::OverCap,
                    ImplementationsAnswer {
                        reason: None,
                        implementations,
                        ..
                    } if !implementations.is_empty() => Followed::Found(implementations),
                    _ => return None,
                };
                Some((call, followed))
            })
            .collect();
        Implementations {
            by_call,
            asked: count,
        }
    }

    /// What the sidecar found for the call at `lo..hi` of `file`.
    pub(super) fn at(&self, file: &Path, lo: u32, hi: u32) -> Option<&Followed> {
        self.by_call.get(&(file.to_path_buf(), lo, hi))
    }

    /// How many calls hold an answer.
    pub fn answered(&self) -> usize {
        self.by_call.len()
    }
}

/// Ask `ask` the implementations of every call of `inputs` through an
/// interface member, once. `ask` gets the questions in order and answers one
/// per question; an `Err` or a short answer leaves the calls as stops. No
/// rows, or no such call, asks nothing.
pub fn implementations_of<E: std::fmt::Display>(
    inputs: &RequestSummaryInputs,
    ask: impl FnOnce(&[ImplementationsQuery]) -> Result<Vec<ImplementationsAnswer>, E>,
) -> Implementations {
    let (calls, queries): (Vec<_>, Vec<_>) = inputs.implementation_queries().into_iter().unzip();
    if queries.is_empty() {
        return Implementations::default();
    }
    match ask(&queries) {
        Ok(answers) if answers.len() == queries.len() => Implementations::of(calls, answers),
        Ok(answers) => {
            tracing::warn!(
                asked = queries.len(),
                answered = answers.len(),
                "implementations: the answer does not match the question; none read"
            );
            Implementations {
                asked: queries.len(),
                ..Default::default()
            }
        }
        Err(error) => {
            tracing::warn!(%error, "implementations: the ask failed; calls through an interface member stay stops");
            Implementations {
                asked: queries.len(),
                ..Default::default()
            }
        }
    }
}

impl RequestSummaryInputs {
    /// One question for each call whose declaration is an interface member
    /// the service writes ([`Self::unfollowed_at`]): the call's file as
    /// walked and SWC span, and the span of its `call` row in the rows'
    /// numbering. Sorted by file and span.
    pub(super) fn implementation_queries(
        &self,
    ) -> Vec<((PathBuf, u32, u32), ImplementationsQuery)> {
        let Some(positions) = self.positions.as_ref() else {
            return Vec::new();
        };
        let mut out: Vec<((PathBuf, u32, u32), ImplementationsQuery)> = every_call(&self.files)
            .into_iter()
            .filter(|(_, call, construction)| !construction && !call.jsx)
            .filter(|(file, call, _)| {
                self.unfollowed_at(file, call.site.lo, call.site.hi)
                    .is_some_and(|stop| stop.reason == Stop::InterfaceMember)
            })
            .filter_map(|(file, call, _)| {
                let ir = self.files.get(file)?;
                let row = positions
                    .lookup(
                        file,
                        ir.swc_start(call.site.lo),
                        ir.swc_start(call.site.hi),
                        Kind::Call,
                    )
                    .ok()?;
                Some((
                    (file.to_path_buf(), call.site.lo, call.site.hi),
                    ImplementationsQuery {
                        file: file.to_string_lossy().into_owned(),
                        start: row.start(),
                        end: row.end(),
                    },
                ))
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out.dedup_by(|a, b| a.0 == b.0);
        out
    }
}
