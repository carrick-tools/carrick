//! Which producer a call reaches: the one pairing rule for every protocol
//! (carrick#3496).
//!
//! Every surface that decides which producer a call pairs with (the
//! scanner's edges, the type check's pairs, the cloud's service graph and
//! tools) asks [`PairingPool::pair`]; none ranks or filters candidates on its
//! own. The rule, and the cases that pin it, are the language-neutral vector
//! file `crates/carrick-match/tests/fixtures/pairing.vectors.json`, run by
//! `tests/pairing_vectors.rs` natively and by the wasm smoke in
//! `.github/workflows/ci.yml` over the wasm build the cloud pins.
//!
//! - GraphQL, socket and pub/sub: a producer pairs when protocol, name and
//!   (socket) direction are equal and [`names_pair`] holds. Every such
//!   producer is a winner; there is no ranking.
//! - HTTP: an unknown call path pairs with nothing; otherwise the pool is
//!   every HTTP producer, in every service including the caller's, whose
//!   method serves the call's ([`method_matches`]); only the producers with
//!   maximal [`match_agreement`] are kept; each answers
//!   [`match_verdict_with_dispatch`], and [`dispatch_outcome`] over those
//!   answers decides the call.
//!
//! Inputs are stored keys and are never rewritten. [`canonical_path`] is the
//! form a writer stores.

use std::collections::HashMap;

use crate::{
    ANY_METHOD, DispatchOutcome, MatchEvidence, MatchRelationship, MatchVerdict, NameScope,
    ScopedRow, classify_relationship, dispatch_outcome, is_literal_segment, is_param_segment,
    is_unknown_call_path, match_agreement, match_verdict_with_dispatch, names_pair,
};

/// The protocol label of an HTTP row. Every other label pairs by exact key.
const HTTP: &str = "http";

/// One producer row in the pool: a route, an SDL root field, a socket
/// listener or a pub/sub subscriber, as the index stores it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct Producer {
    pub service: String,
    /// `http`, `graphql`, `socket` or `pubsub`.
    pub protocol: String,
    /// HTTP only. [`ANY_METHOD`] serves every method.
    pub method: Option<String>,
    /// HTTP only: the route's full path as stored.
    pub path: Option<String>,
    /// Non-HTTP: the operation key's name as the index carries it.
    pub name: Option<String>,
    /// Socket only.
    pub direction: Option<String>,
    /// HTTP only: the [`crate::dispatch_key`] of the case this producer
    /// answers, when its handler dispatches on a request field.
    pub dispatch: Option<String>,
    pub evidence: MatchEvidence,
    pub name_scope: Option<NameScope>,
}

/// One call: an HTTP request, a GraphQL operation, a socket emit or a
/// pub/sub publish.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct Call {
    pub service: String,
    pub protocol: String,
    pub method: Option<String>,
    /// HTTP only: the call's canonical path.
    pub path: Option<String>,
    pub name: Option<String>,
    pub direction: Option<String>,
    /// HTTP only: the dispatch value the call states, as a
    /// [`crate::dispatch_key`].
    pub dispatch: Option<String>,
    pub name_scope: Option<NameScope>,
}

/// What a call is to the pool as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum PairOutcome {
    /// At least one winner, each [`MatchVerdict::Matched`].
    Matched,
    /// The call reaches a dispatching route without stating its case: the
    /// winners are the route's cases, each
    /// [`MatchVerdict::DispatchValueUnknown`], and a surface draws one edge
    /// to the route with the case unknown (carrick#2048). Never missing.
    RouteCaseUnknown,
    /// The call states a dispatch value no case answers. No winner.
    NoCaseAnswers,
    /// The call path has no literal segment ([`is_unknown_call_path`]): an
    /// extraction gap, paired with nothing.
    UnknownCallPath,
    /// The best route corroborates nothing (a bare catch-all or a
    /// param-only route). No winner, and the call is not a missing endpoint.
    NoLiteralAgreement,
    /// No producer routes the call.
    NoRoute,
}

/// One producer the call pairs with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Winner {
    /// Index into the pool's input order.
    pub producer: usize,
    /// [`MatchVerdict::Matched`], or [`MatchVerdict::DispatchValueUnknown`]
    /// under [`PairOutcome::RouteCaseUnknown`].
    pub verdict: MatchVerdict,
    /// HTTP only: the literal agreement the winner ranked on.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub agreement: Option<u32>,
    /// The producer's service is the call's.
    pub same_service: bool,
    pub relationship: MatchRelationship,
    /// HTTP only: the call has a placeholder where this route states a
    /// literal (carrick#2004). The pair stands; the type check does not
    /// compare it.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub call_param_over_route_literal: Option<bool>,
}

/// One maximal-agreement HTTP producer and its verdict, winner or not: what
/// a surface reads for a reason or a near miss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Routed {
    pub producer: usize,
    pub verdict: MatchVerdict,
    pub agreement: u32,
}

/// The answer for one call. `winners` and `routed` are in producer input
/// order; `routed` is empty for non-HTTP calls, an unknown call path and
/// [`PairOutcome::NoRoute`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Pairing {
    pub outcome: PairOutcome,
    pub winners: Vec<Winner>,
    pub routed: Vec<Routed>,
}

impl Pairing {
    fn empty(outcome: PairOutcome) -> Self {
        Pairing {
            outcome,
            winners: Vec::new(),
            routed: Vec::new(),
        }
    }
}

/// The producers, indexed once so that each call asks only its candidates.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
pub struct PairingPool {
    producers: Vec<Producer>,
    /// HTTP producers by upper-cased method, in input order. [`ANY_METHOD`]
    /// is one of the keys.
    http_by_method: HashMap<String, Vec<usize>>,
    /// Non-HTTP producers by (protocol, name, direction), in input order.
    by_key: HashMap<(String, String, Option<String>), Vec<usize>>,
}

fn method_key(method: &str) -> String {
    method.trim().to_ascii_uppercase()
}

impl PairingPool {
    pub fn new(producers: Vec<Producer>) -> Self {
        let mut http_by_method: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_key: HashMap<(String, String, Option<String>), Vec<usize>> = HashMap::new();
        for (index, producer) in producers.iter().enumerate() {
            if producer.protocol == HTTP {
                if let Some(method) = &producer.method {
                    http_by_method
                        .entry(method_key(method))
                        .or_default()
                        .push(index);
                }
            } else if let Some(name) = &producer.name {
                by_key
                    .entry((
                        producer.protocol.clone(),
                        name.clone(),
                        producer.direction.clone(),
                    ))
                    .or_default()
                    .push(index);
            }
        }
        PairingPool {
            producers,
            http_by_method,
            by_key,
        }
    }

    /// The producers, in the order the pool was built from.
    pub fn producers(&self) -> &[Producer] {
        &self.producers
    }

    /// Which producers `call` reaches, and what the call is to the pool.
    pub fn pair(&self, call: &Call) -> Pairing {
        if call.protocol == HTTP {
            self.pair_http(call)
        } else {
            self.pair_exact_key(call)
        }
    }

    fn pair_exact_key(&self, call: &Call) -> Pairing {
        let Some(name) = &call.name else {
            return Pairing::empty(PairOutcome::NoRoute);
        };
        let key = (call.protocol.clone(), name.clone(), call.direction.clone());
        let call_row = ScopedRow {
            service: &call.service,
            name_scope: call.name_scope.as_ref(),
        };
        let winners: Vec<Winner> = self
            .by_key
            .get(&key)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&index| {
                let producer = &self.producers[index];
                names_pair(
                    call_row,
                    ScopedRow {
                        service: &producer.service,
                        name_scope: producer.name_scope.as_ref(),
                    },
                )
            })
            .map(|index| self.winner(index, call, MatchVerdict::Matched, None))
            .collect();
        let outcome = if winners.is_empty() {
            PairOutcome::NoRoute
        } else {
            PairOutcome::Matched
        };
        Pairing {
            outcome,
            winners,
            routed: Vec::new(),
        }
    }

    /// The HTTP producers whose method serves `call_method`, in input order:
    /// its own method's routes and the any-method routes ([`method_matches`]
    /// answers the same; this is its indexed form). A call made with
    /// [`ANY_METHOD`] reaches only the any-method routes.
    ///
    /// [`method_matches`]: crate::method_matches
    fn http_candidates(&self, call_method: &str) -> Vec<usize> {
        let own = method_key(call_method);
        let mut candidates: Vec<usize> = self
            .http_by_method
            .get(&own)
            .into_iter()
            .flatten()
            .copied()
            .collect();
        if own != ANY_METHOD {
            candidates.extend(self.http_by_method.get(ANY_METHOD).into_iter().flatten());
            candidates.sort_unstable();
        }
        candidates
    }

    fn pair_http(&self, call: &Call) -> Pairing {
        let call_path = call.path.as_deref().unwrap_or("");
        if is_unknown_call_path(call_path) {
            return Pairing::empty(PairOutcome::UnknownCallPath);
        }
        let Some(call_method) = &call.method else {
            return Pairing::empty(PairOutcome::NoRoute);
        };
        let scored: Vec<(usize, u32)> = self
            .http_candidates(call_method)
            .into_iter()
            .filter_map(|index| {
                let path = self.producers[index].path.as_deref()?;
                match_agreement(path, call_path).map(|agreement| (index, agreement))
            })
            .collect();
        let Some(best) = scored.iter().map(|&(_, agreement)| agreement).max() else {
            return Pairing::empty(PairOutcome::NoRoute);
        };

        let mut routed = Vec::new();
        let mut matched = Vec::new();
        let mut case_unknown = Vec::new();
        let mut case_mismatched = 0u32;
        for (index, agreement) in scored.into_iter().filter(|&(_, a)| a == best) {
            let producer = &self.producers[index];
            let verdict = match_verdict_with_dispatch(
                producer.path.as_deref().unwrap_or(""),
                call_path,
                producer.dispatch.clone(),
                call.dispatch.clone(),
            );
            routed.push(Routed {
                producer: index,
                verdict,
                agreement,
            });
            match verdict {
                MatchVerdict::Matched => {
                    matched.push(self.winner(index, call, verdict, Some(agreement)))
                }
                MatchVerdict::DispatchValueUnknown => {
                    case_unknown.push(self.winner(index, call, verdict, Some(agreement)))
                }
                MatchVerdict::DispatchValueMismatch => case_mismatched += 1,
                _ => {}
            }
        }
        let (outcome, winners) = match dispatch_outcome(
            matched.len() as u32,
            case_unknown.len() as u32,
            case_mismatched,
        ) {
            DispatchOutcome::Matched => (PairOutcome::Matched, matched),
            DispatchOutcome::RouteCaseUnknown => (PairOutcome::RouteCaseUnknown, case_unknown),
            DispatchOutcome::NoCaseAnswers => (PairOutcome::NoCaseAnswers, Vec::new()),
            DispatchOutcome::NotMatched => (PairOutcome::NoLiteralAgreement, Vec::new()),
        };
        Pairing {
            outcome,
            winners,
            routed,
        }
    }

    fn winner(
        &self,
        index: usize,
        call: &Call,
        verdict: MatchVerdict,
        agreement: Option<u32>,
    ) -> Winner {
        let producer = &self.producers[index];
        let call_param_over_route_literal = (producer.protocol == HTTP).then(|| {
            call_param_over_route_literal(
                producer.path.as_deref().unwrap_or(""),
                call.path.as_deref().unwrap_or(""),
            )
        });
        Winner {
            producer: index,
            verdict,
            agreement,
            same_service: producer.service == call.service,
            relationship: classify_relationship(producer.evidence, MatchEvidence::CallSite),
            call_param_over_route_literal,
        }
    }
}

/// [`PairingPool::pair`] over a pool built for one call. A surface pairing
/// more than one call builds the pool once instead.
pub fn pair_winners(producers: &[Producer], call: &Call) -> Pairing {
    PairingPool::new(producers.to_vec()).pair(call)
}

/// Whether the call has a placeholder at a position where the route states
/// a literal (carrick#2004): the call's value there is unknown, so the route
/// is reached but its literal is not confirmed.
fn call_param_over_route_literal(route_path: &str, call_path: &str) -> bool {
    route_path
        .split('/')
        .zip(call_path.split('/'))
        .any(|(route, call)| {
            is_param_segment(call) && is_literal_segment(route.trim_end_matches('?'))
        })
}

/// The form a writer stores a path in: one leading `/`, no empty segment
/// (`//` collapsed), no trailing `/` except the root, and `/` for an empty
/// path. Case and `?` optional markers are kept. Pairing never applies it:
/// it compares stored keys as written.
#[cfg_attr(feature = "wasm", wasm_bindgen::prelude::wasm_bindgen)]
pub fn canonical_path(path: &str) -> String {
    let segments: Vec<&str> = path.split('/').filter(|seg| !seg.is_empty()).collect();
    format!("/{}", segments.join("/"))
}

#[cfg(feature = "wasm")]
mod wasm {
    use wasm_bindgen::prelude::*;

    use super::{Call, PairingPool, Producer};

    /// The pool over wasm: JSON in, JSON out, in the vector file's shape.
    #[wasm_bindgen]
    impl PairingPool {
        /// `producers` is a JSON `Producer[]`. Throws on malformed JSON.
        #[wasm_bindgen(constructor)]
        pub fn from_json(producers: &str) -> Result<PairingPool, JsError> {
            let producers: Vec<Producer> = serde_json::from_str(producers)?;
            Ok(PairingPool::new(producers))
        }

        /// `call` is a JSON `Call`; the result is a JSON `Pairing`. Throws on
        /// malformed JSON.
        #[wasm_bindgen(js_name = pair)]
        pub fn pair_json(&self, call: &str) -> Result<String, JsError> {
            let call: Call = serde_json::from_str(call)?;
            Ok(serde_json::to_string(&self.pair(&call))?)
        }
    }
}
