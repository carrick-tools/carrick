//! The pool splits each path once (the routes in `PairingPool::new`, the
//! call once per `pair`) and pairs exactly as the matcher that split both
//! paths again for every candidate route.
//!
//! `frozen` below is that matcher, copied from carrick c8c654a7
//! (`crates/carrick-match/src/lib.rs` and `src/pairing.rs`): the string
//! path functions and the HTTP arm of `PairingPool::pair`. It is the oracle
//! for this test only and is never called by the crate. Every HTTP call of
//! an invented fixture (params in four spellings, optional segments,
//! wildcards, version segments, trailing and doubled slashes, blank
//! segments, method case, `ALL`, dispatch cases, non-HTTP rows between the
//! routes) must get the same whole `Pairing` from both, and every public
//! path function must give the same answer over the cross product.
//!
//! `split_once_timing_log` prints the per-call time of both over the same
//! fixture. It asserts nothing about time.
//!
//! The oracle has learned two rules since c8c654a7. The named catch-all of
//! carrick#4205 (`*name`, `{*name}`, a trailing `{/*name}`): `split_route`,
//! `catch_all_prefix` and `is_named_wildcard` below are copies of the
//! crate's, so the parity proof still covers named wildcards. And
//! carrick#4262's unconfirmed literals: `match_verdict` rejects a pair that
//! leaves more route literals to a placeholder than it confirms, and
//! `Pool::pair_http` keeps only the maximal-agreement routes with the fewest
//! of them.

use std::time::Instant;

use carrick_match::{
    Call, MatchEvidence, Pairing, PairingPool, Producer, is_unknown_call_path, match_agreement,
    match_verdict, match_verdict_with_dispatch, path_literal_specificity, paths_match,
    reportable_agreement, reportable_agreement_with_dispatch,
};

/// The c8c654a7 matcher, verbatim but for `pub`, the pool's shape and the
/// carrick#4205 named catch-all.
mod frozen {
    use std::collections::HashMap;

    use carrick_match::{
        ANY_METHOD, DispatchOutcome, DispatchVerdict, MatchEvidence, MatchVerdict, PairOutcome,
        Pairing, Producer, Routed, Winner, classify_relationship, dispatch_outcome,
        dispatch_verdict, is_param_segment,
    };

    pub fn match_agreement(endpoint_path: &str, call_path: &str) -> Option<u32> {
        if endpoint_path == call_path {
            return Some(path_literal_specificity(endpoint_path));
        }
        agreement_with_params(endpoint_path, call_path)
            .or_else(|| agreement_with_wildcards(endpoint_path, call_path))
    }

    /// carrick#4205: `split('/')`, with a trailing `{/*name}` read as the
    /// segment `*name` after its prefix.
    fn split_route(path: &str) -> impl Iterator<Item = &str> {
        let (head, tail) = match optional_wildcard_tail(path) {
            Some(open) => {
                let above = &path[..open];
                (
                    above.strip_suffix('/').unwrap_or(above),
                    Some(&path[open + 2..path.len() - 1]),
                )
            }
            None => (path, None),
        };
        head.split('/').chain(tail)
    }

    fn optional_wildcard_tail(path: &str) -> Option<usize> {
        let body = path.strip_suffix('}')?;
        let open = body.rfind("{/*")?;
        is_wildcard_name(&body[open + 3..]).then_some(open)
    }

    fn is_wildcard_name(name: &str) -> bool {
        let mut chars = name.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
    }

    fn is_named_wildcard(seg: &str) -> bool {
        let inner = seg
            .strip_prefix('{')
            .and_then(|s| s.strip_suffix('}'))
            .unwrap_or(seg);
        inner.strip_prefix('*').is_some_and(is_wildcard_name)
    }

    fn catch_all_prefix(path: &str) -> Option<&str> {
        if path.ends_with("/*") || path.ends_with("/**") {
            return Some(path.trim_end_matches("/**").trim_end_matches("/*"));
        }
        if path.ends_with("/(.*)") {
            return Some(path.trim_end_matches("/(.*)"));
        }
        if let Some(open) = optional_wildcard_tail(path) {
            let above = &path[..open];
            return Some(above.strip_suffix('/').unwrap_or(above));
        }
        let (above, last) = path.rsplit_once('/')?;
        is_named_wildcard(last).then_some(above)
    }

    pub fn path_literal_specificity(path: &str) -> u32 {
        split_route(path)
            .filter(|seg| is_literal_segment(seg.trim_end_matches('?')))
            .count() as u32
    }

    pub fn is_catch_all_path(path: &str) -> bool {
        catch_all_prefix(path).is_some()
    }

    pub fn is_unknown_call_path(call_path: &str) -> bool {
        path_literal_specificity(call_path) == 0
    }

    pub fn match_verdict_with_dispatch(
        endpoint_path: &str,
        call_path: &str,
        producer_dispatch: Option<String>,
        call_dispatch: Option<String>,
    ) -> MatchVerdict {
        match match_verdict(endpoint_path, call_path) {
            MatchVerdict::Matched => match dispatch_verdict(producer_dispatch, call_dispatch) {
                DispatchVerdict::NotDispatching | DispatchVerdict::Matched => MatchVerdict::Matched,
                DispatchVerdict::ValueUnknown => MatchVerdict::DispatchValueUnknown,
                DispatchVerdict::ValueMismatch => MatchVerdict::DispatchValueMismatch,
            },
            rejected => rejected,
        }
    }

    pub fn reportable_agreement_with_dispatch(
        endpoint_path: &str,
        call_path: &str,
        producer_dispatch: Option<String>,
        call_dispatch: Option<String>,
    ) -> Option<u32> {
        match match_verdict_with_dispatch(
            endpoint_path,
            call_path,
            producer_dispatch,
            call_dispatch,
        ) {
            MatchVerdict::Matched => match_agreement(endpoint_path, call_path),
            _ => None,
        }
    }

    pub fn match_verdict(endpoint_path: &str, call_path: &str) -> MatchVerdict {
        if is_unknown_call_path(call_path) {
            return MatchVerdict::UnknownCallPath;
        }
        if is_catch_all_path(endpoint_path) && path_literal_specificity(endpoint_path) == 0 {
            return MatchVerdict::CatchAllRoute;
        }
        match match_agreement(endpoint_path, call_path) {
            None => MatchVerdict::NoRouteMatch,
            Some(0) => MatchVerdict::NoLiteralAgreement,
            Some(agreement) if unconfirmed_literals(endpoint_path, call_path) > agreement => {
                MatchVerdict::NoLiteralAgreement
            }
            Some(_) => MatchVerdict::Matched,
        }
    }

    pub fn reportable_agreement(endpoint_path: &str, call_path: &str) -> Option<u32> {
        match match_verdict(endpoint_path, call_path) {
            MatchVerdict::Matched => match_agreement(endpoint_path, call_path),
            _ => None,
        }
    }

    fn is_literal_segment(seg: &str) -> bool {
        !seg.trim().is_empty() && !is_param_segment(seg) && !is_wildcard_segment(seg)
    }

    fn is_wildcard_segment(seg: &str) -> bool {
        seg == "*" || seg == "**" || seg == "(.*)" || is_named_wildcard(seg)
    }

    fn agreement_with_params(endpoint_path: &str, call_path: &str) -> Option<u32> {
        let endpoint_segments: Vec<&str> = split_route(endpoint_path).collect();
        let call_segments: Vec<&str> = split_route(call_path).collect();
        let endpoint_required_count = endpoint_segments
            .iter()
            .filter(|s| !s.ends_with('?'))
            .count();
        if call_segments.len() < endpoint_required_count
            || call_segments.len() > endpoint_segments.len()
        {
            return None;
        }
        let mut agreement = 0u32;
        for (i, endpoint_seg) in endpoint_segments.iter().enumerate() {
            let is_optional = endpoint_seg.ends_with('?');
            let seg = endpoint_seg.trim_end_matches('?');
            if i >= call_segments.len() {
                if !is_optional {
                    return None;
                }
                continue;
            }
            let call_seg = call_segments[i];
            if is_param_segment(seg) || is_param_segment(call_seg) {
                if seg.trim().is_empty() || call_seg.trim().is_empty() {
                    return None;
                }
                continue;
            }
            if seg != call_seg {
                return None;
            }
            if is_literal_segment(seg) {
                agreement += 1;
            }
        }
        Some(agreement)
    }

    fn agreement_with_wildcards(endpoint_path: &str, call_path: &str) -> Option<u32> {
        fn prefix_matches_on_boundary(prefix: &str, call_path: &str) -> bool {
            match call_path.strip_prefix(prefix) {
                Some(rest) => rest.is_empty() || rest.starts_with('/'),
                None => false,
            }
        }
        if let Some(prefix) = catch_all_prefix(endpoint_path) {
            return prefix_matches_on_boundary(prefix, call_path)
                .then(|| path_literal_specificity(prefix));
        }
        let endpoint_segments: Vec<&str> = split_route(endpoint_path).collect();
        let call_segments: Vec<&str> = split_route(call_path).collect();
        if endpoint_segments.len() != call_segments.len() {
            return None;
        }
        let mut agreement = 0u32;
        for (endpoint_seg, call_seg) in endpoint_segments.iter().zip(call_segments.iter()) {
            if *endpoint_seg == "*" || is_named_wildcard(endpoint_seg) {
                continue;
            }
            if endpoint_seg != call_seg {
                return None;
            }
            if is_literal_segment(endpoint_seg) {
                agreement += 1;
            }
        }
        Some(agreement)
    }

    fn unconfirmed_literals(route_path: &str, call_path: &str) -> u32 {
        split_route(route_path)
            .zip(split_route(call_path))
            .filter(|(route, call)| {
                is_param_segment(call) && is_literal_segment(route.trim_end_matches('?'))
            })
            .count() as u32
    }

    fn call_param_over_route_literal(route_path: &str, call_path: &str) -> bool {
        unconfirmed_literals(route_path, call_path) > 0
    }

    fn method_key(method: &str) -> String {
        method.trim().to_ascii_uppercase()
    }

    /// The HTTP half of the c8c654a7 `PairingPool`.
    pub struct Pool {
        producers: Vec<Producer>,
        http_by_method: HashMap<String, Vec<usize>>,
    }

    impl Pool {
        pub fn new(producers: Vec<Producer>) -> Self {
            let mut http_by_method: HashMap<String, Vec<usize>> = HashMap::new();
            for (index, producer) in producers.iter().enumerate() {
                if producer.protocol == "http" {
                    if let Some(method) = &producer.method {
                        http_by_method
                            .entry(method_key(method))
                            .or_default()
                            .push(index);
                    }
                }
            }
            Pool {
                producers,
                http_by_method,
            }
        }

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

        /// `PairingPool::pair_http` at c8c654a7, for a call without a
        /// procedure.
        pub fn pair_http(&self, call: &carrick_match::Call) -> Pairing {
            let empty = |outcome| Pairing {
                outcome,
                winners: Vec::new(),
                routed: Vec::new(),
            };
            let call_path = call.path.as_deref().unwrap_or("");
            if is_unknown_call_path(call_path) {
                return empty(PairOutcome::UnknownCallPath);
            }
            let Some(call_method) = &call.method else {
                return empty(PairOutcome::NoRoute);
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
                return empty(PairOutcome::NoRoute);
            };
            let at_best: Vec<(usize, u32)> =
                scored.into_iter().filter(|&(_, a)| a == best).collect();
            let unconfirmed = |index: usize| {
                unconfirmed_literals(
                    self.producers[index].path.as_deref().unwrap_or(""),
                    call_path,
                )
            };
            let fewest = at_best
                .iter()
                .map(|&(index, _)| unconfirmed(index))
                .min()
                .unwrap_or(0);
            let mut routed = Vec::new();
            let mut matched = Vec::new();
            let mut case_unknown = Vec::new();
            let mut case_mismatched = 0u32;
            for (index, agreement) in at_best
                .into_iter()
                .filter(|&(index, _)| unconfirmed(index) == fewest)
            {
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
            call: &carrick_match::Call,
            verdict: MatchVerdict,
            agreement: Option<u32>,
        ) -> Winner {
            let producer = &self.producers[index];
            let call_param_over_route_literal =
                (call.procedure.is_none() && producer.protocol == "http").then(|| {
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
}

/// A deterministic xorshift, so the fixture is the same on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as usize
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.next() % items.len()]
    }
}

/// Segments a route may carry: literals with version segments, the four
/// param spellings and a rest param, optional markers, every wildcard
/// token, the named wildcards of carrick#4205, a mixed template
/// segment, a lookalike prefix and blank segments.
const ROUTE_SEGMENTS: &[&str] = &[
    "api",
    "api2",
    "v1",
    "v2",
    "users",
    "orders",
    "health",
    ":id",
    "{id}",
    "<id>",
    "[id]",
    "[...slug]",
    ":id?",
    "v1?",
    "orders?",
    "*",
    "**",
    "(.*)",
    "*slug",
    "*rest",
    "{*rest}",
    "v${n}",
    "",
    " ",
];

/// Segments a call may carry: literals, concrete values, placeholders a
/// writer normalised a variable to, blanks and stray wildcards.
const CALL_SEGMENTS: &[&str] = &[
    "api", "api2", "v1", "v2", "users", "orders", "health", "123", ":id", ":userId", "{id}",
    "[id]", "*", "", " ", "v${n}",
];

/// Whole paths no generator draws: the odd shapes a stored key can take.
const ODD_PATHS: &[&str] = &[
    "",
    "/",
    "//",
    "///",
    "users",
    "api/users",
    "/api/users/",
    "/api//users",
    "/api/**/**",
    "/api/*/**",
    "/api/**/*",
    "/api/(.*)/(.*)",
    "/*",
    "/**",
    "/(.*)",
    "*",
    "/:id",
    "/users/:id?/orders",
    "/users/:id?/:tab?",
    "/v1/users/:id",
    "/v2/users/:id",
    "/api/v1/users/:id/orders",
    "/api/v1/users/123/orders",
    "/v1{/*rest}",
    "/{/*rest}",
];

fn path(rng: &mut Rng, alphabet: &[&str]) -> String {
    let length = 1 + rng.next() % 4;
    let segments: Vec<&str> = (0..length).map(|_| rng.pick(alphabet)).collect();
    let mut path = format!("/{}", segments.join("/"));
    if rng.next() % 9 == 0 {
        path.push('/');
    }
    path
}

fn paths(rng: &mut Rng, alphabet: &[&str], count: usize) -> Vec<String> {
    let mut paths: Vec<String> = ODD_PATHS.iter().map(|path| path.to_string()).collect();
    // Every one- and two-segment path over the alphabet, then drawn ones.
    for first in alphabet {
        paths.push(format!("/{first}"));
        for second in alphabet.iter().step_by(3) {
            paths.push(format!("/{first}/{second}"));
        }
    }
    while paths.len() < count {
        paths.push(path(rng, alphabet));
    }
    paths
}

const PRODUCER_METHODS: &[Option<&str>] = &[
    Some("GET"),
    Some("get"),
    Some(" POST "),
    Some("ALL"),
    Some("all"),
    Some("PUT"),
    None,
];
const CALL_METHODS: &[Option<&str>] = &[Some("GET"), Some("post"), Some("ALL"), Some("put"), None];
const DISPATCHES: &[Option<&str>] = &[
    None,
    None,
    None,
    Some("body:action=a"),
    Some("body:action=b"),
    Some("query:op=a"),
    Some("nosep"),
];

fn producers(rng: &mut Rng) -> Vec<Producer> {
    let mut producers = Vec::new();
    for (index, path) in paths(rng, ROUTE_SEGMENTS, 260).into_iter().enumerate() {
        // A socket row and a procedure-free GraphQL row between the routes
        // shift every HTTP index off its position in the path list.
        if index % 7 == 0 {
            producers.push(Producer {
                service: "svc-a".into(),
                protocol: "socket".into(),
                name: Some(path.clone()),
                direction: Some("client_to_server".into()),
                ..Producer::default()
            });
        }
        if index % 11 == 0 {
            producers.push(Producer {
                service: "svc-b".into(),
                protocol: "graphql".into(),
                name: Some("query|orders".into()),
                ..Producer::default()
            });
        }
        producers.push(Producer {
            service: if index % 2 == 0 { "svc-a" } else { "svc-b" }.into(),
            protocol: "http".into(),
            method: PRODUCER_METHODS[rng.next() % PRODUCER_METHODS.len()].map(String::from),
            // One route in 23 has no path.
            path: (index % 23 != 5).then_some(path),
            dispatch: DISPATCHES[rng.next() % DISPATCHES.len()].map(String::from),
            evidence: if index % 5 == 0 {
                MatchEvidence::CallSite
            } else {
                MatchEvidence::RouteDefinition
            },
            ..Producer::default()
        });
    }
    producers
}

/// Calls aimed at a route: the route as written, with its trailing slash
/// toggled, with every placeholder and wildcard made a concrete value, and
/// that with one more segment (what a catch-all absorbs). Drawn calls
/// rarely land on a route; these do, so every arm is reached on a route
/// that pairs.
fn aimed_at(route: &str) -> [String; 4] {
    let toggled = match route.strip_suffix('/') {
        Some(stripped) => stripped.to_string(),
        None => format!("{route}/"),
    };
    let concrete = route
        .split('/')
        .map(|seg| match seg.trim_end_matches('?') {
            seg if carrick_match::is_param_segment(seg) => "123",
            seg if carrick_match::is_wildcard_segment(seg) => "x",
            _ => seg,
        })
        .collect::<Vec<_>>()
        .join("/");
    let longer = format!("{concrete}/tail");
    [route.to_string(), toggled, concrete, longer]
}

fn calls(rng: &mut Rng, producers: &[Producer]) -> Vec<Call> {
    let drawn: Vec<Call> = paths(rng, CALL_SEGMENTS, 240)
        .into_iter()
        .enumerate()
        .map(|(index, path)| Call {
            service: if index % 3 == 0 { "svc-a" } else { "svc-c" }.into(),
            protocol: "http".into(),
            method: CALL_METHODS[rng.next() % CALL_METHODS.len()].map(String::from),
            path: (index % 29 != 3).then_some(path),
            dispatch: DISPATCHES[rng.next() % DISPATCHES.len()].map(String::from),
            ..Call::default()
        })
        .collect();
    // Every route with a method gets the calls aimed at it, made with its
    // own method, half with its own dispatch case and half with a drawn one.
    let mut aimed = Vec::new();
    for (index, producer) in producers.iter().enumerate() {
        let (Some(path), Some(method)) = (&producer.path, &producer.method) else {
            continue;
        };
        if producer.protocol != "http" {
            continue;
        }
        for (form, call_path) in aimed_at(path).into_iter().enumerate() {
            aimed.push(Call {
                service: if (index + form) % 2 == 0 {
                    "svc-a"
                } else {
                    "svc-c"
                }
                .into(),
                protocol: "http".into(),
                method: Some(method.clone()),
                path: Some(call_path),
                dispatch: if form % 2 == 0 {
                    producer.dispatch.clone()
                } else {
                    DISPATCHES[rng.next() % DISPATCHES.len()].map(String::from)
                },
                ..Call::default()
            });
        }
    }
    drawn.into_iter().chain(aimed).collect()
}

fn fixture() -> (Vec<Producer>, Vec<Call>) {
    let mut rng = Rng(0x5eed_0f_c8c6_54a7);
    let producers = producers(&mut rng);
    let calls = calls(&mut rng, &producers);
    (producers, calls)
}

#[test]
fn the_pool_pairs_every_call_as_the_split_per_candidate_matcher_did() {
    let (producers, calls) = fixture();
    let new = PairingPool::new(producers.clone());
    let old = frozen::Pool::new(producers);
    let mut matched = 0usize;
    for call in &calls {
        let (want, got): (Pairing, Pairing) = (old.pair_http(call), new.pair(call));
        assert_eq!(got, want, "call {call:?}");
        matched += usize::from(!got.winners.is_empty());
    }
    // The fixture reaches the arms it claims to: plenty of calls pair, and
    // plenty do not.
    assert!(
        matched > calls.len() / 10,
        "{matched} of {} paired",
        calls.len()
    );
    assert!(
        matched < calls.len() * 9 / 10,
        "{matched} of {} paired",
        calls.len()
    );
}

#[test]
fn every_public_path_function_answers_as_before() {
    let (producers, calls) = fixture();
    let routes: Vec<(&str, Option<String>)> = producers
        .iter()
        .filter_map(|producer| Some((producer.path.as_deref()?, producer.dispatch.clone())))
        .collect();
    for call in &calls {
        let call_path = call.path.as_deref().unwrap_or("");
        assert_eq!(
            is_unknown_call_path(call_path),
            frozen::is_unknown_call_path(call_path)
        );
        for (route, dispatch) in &routes {
            let pair = format!("{route:?} x {call_path:?}");
            assert_eq!(
                match_agreement(route, call_path),
                frozen::match_agreement(route, call_path),
                "{pair}"
            );
            assert_eq!(
                paths_match(route, call_path),
                frozen::match_agreement(route, call_path).is_some(),
                "{pair}"
            );
            assert_eq!(
                match_verdict(route, call_path),
                frozen::match_verdict(route, call_path),
                "{pair}"
            );
            assert_eq!(
                reportable_agreement(route, call_path),
                frozen::reportable_agreement(route, call_path),
                "{pair}"
            );
            assert_eq!(
                match_verdict_with_dispatch(
                    route,
                    call_path,
                    dispatch.clone(),
                    call.dispatch.clone()
                ),
                frozen::match_verdict_with_dispatch(
                    route,
                    call_path,
                    dispatch.clone(),
                    call.dispatch.clone()
                ),
                "{pair}"
            );
            assert_eq!(
                reportable_agreement_with_dispatch(
                    route,
                    call_path,
                    dispatch.clone(),
                    call.dispatch.clone()
                ),
                frozen::reportable_agreement_with_dispatch(
                    route,
                    call_path,
                    dispatch.clone(),
                    call.dispatch.clone()
                ),
                "{pair}"
            );
        }
        assert_eq!(
            path_literal_specificity(call_path),
            frozen::path_literal_specificity(call_path)
        );
    }
}

/// Prints the per-call time of the frozen matcher and the pool over the
/// fixture, each the best of five passes. Read it with
/// `cargo test -p carrick-match --test split_once_parity -- --nocapture`.
#[test]
fn split_once_timing_log() {
    let (producers, calls) = fixture();
    let new = PairingPool::new(producers.clone());
    let old = frozen::Pool::new(producers);
    let best = |pair: &dyn Fn(&Call) -> Pairing| {
        (0..5)
            .map(|_| {
                let start = Instant::now();
                for call in &calls {
                    std::hint::black_box(pair(call));
                }
                start.elapsed()
            })
            .min()
            .expect("five passes")
    };
    let old_time = best(&|call| old.pair_http(call));
    let new_time = best(&|call| new.pair(call));
    let per_call = |time: std::time::Duration| time.as_secs_f64() * 1e6 / calls.len() as f64;
    println!(
        "split_once_timing: {} calls over {} routes; split per candidate {:.1} us/call, \
         split once {:.1} us/call, ratio {:.2}x",
        calls.len(),
        new.producers().len(),
        per_call(old_time),
        per_call(new_time),
        old_time.as_secs_f64() / new_time.as_secs_f64(),
    );
}
