//! What the cloud has said it can do, as this run has heard it
//! (carrick#1897).
//!
//! A prompt lambda may put `X-Carrick-Capabilities` on the responses it
//! builds itself: a comma-separated list of tokens, each naming something a
//! request may ask of it. The scanner reads one, `collect`: the lambda
//! answers a request carrying `X-Carrick-Collect` only from what an earlier
//! request left, and never by a model call of its own.
//!
//! The scanner sends such a request only to a route that has stated the token
//! in this run. A cloud that does not read `X-Carrick-Collect` treats the
//! request as an ordinary one and may call the model for it, and nothing
//! else tells the two clouds apart beforehand: the scanner-version gate is
//! the cloud refusing a scanner, and every check that refuses a request
//! before the model runs the same on both. So the rule holds by what the
//! cloud said and not by the order two releases went out in.
//!
//! Heard per route and API base, because the statement is the lambda's own:
//! a response the gateway produced (a cut, a throttle) is not the lambda's
//! and states nothing. Kept for the run and never written down, so a deploy
//! between two runs is heard afresh.

use std::collections::HashMap;
use std::sync::Mutex;
use tracing::debug;

/// The response header a prompt lambda lists its capabilities in.
pub(crate) const CAPABILITIES_HEADER: &str = "X-Carrick-Capabilities";

/// The capability a collecting request depends on.
pub(crate) const COLLECT: &str = "collect";

/// Whether a response's headers list `token` as a capability.
///
/// Read leniently, because the list is the cloud's to grow: tokens are
/// compared without regard to case, the whitespace around one is dropped, a
/// token this build has never heard of is passed over, and a value that is
/// not text states nothing. The header may be sent more than once.
pub(crate) fn states(headers: &reqwest::header::HeaderMap, token: &str) -> bool {
    headers
        .get_all(CAPABILITIES_HEADER)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|stated| stated.trim().eq_ignore_ascii_case(token))
}

/// Which endpoints have stated `collect` in this run.
#[derive(Debug, Default)]
pub(crate) struct CloudCapabilities {
    /// By endpoint (API base and route): `true` once an answer of the
    /// lambda's stated `collect`, `false` while its answers have not.
    /// Absent until the lambda has answered at all.
    collect: Mutex<HashMap<String, bool>>,
}

impl CloudCapabilities {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record what one answer the lambda at `endpoint` built said about
    /// `collect`. Once stated it stays stated for the run. The run's log says
    /// which it was the first time the lambda answers, and again if a later
    /// answer is the first to state it.
    pub(crate) fn heard(&self, endpoint: &str, stated: bool) {
        let mut collect = self.collect.lock().expect("cloud capabilities lock");
        let known = collect.get(endpoint).copied();
        if known == Some(true) || known == Some(stated) {
            return;
        }
        collect.insert(endpoint.to_string(), stated);
        debug!("{}", statement_line(endpoint, stated));
    }

    /// Whether `endpoint` has stated `collect` in this run. Never heard, or
    /// heard without it, is no.
    pub(crate) fn collects(&self, endpoint: &str) -> bool {
        self.collect
            .lock()
            .expect("cloud capabilities lock")
            .get(endpoint)
            .copied()
            .unwrap_or(false)
    }

    /// Forget everything, as a run opens.
    pub(crate) fn reset(&self) {
        self.collect
            .lock()
            .expect("cloud capabilities lock")
            .clear();
    }
}

/// The line the run's log gets about one endpoint's `collect`.
fn statement_line(endpoint: &str, stated: bool) -> String {
    if stated {
        format!(
            "{endpoint} states `collect` ({CAPABILITIES_HEADER}): a last attempt the gateway cuts \
             is followed by one request that only collects"
        )
    } else {
        format!(
            "{endpoint} has not stated `collect` ({CAPABILITIES_HEADER}): no collecting request \
             is sent to it, and a last attempt the gateway cuts ends the call"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(values: &[&str]) -> reqwest::header::HeaderMap {
        let mut map = reqwest::header::HeaderMap::new();
        for value in values {
            map.append(
                CAPABILITIES_HEADER,
                reqwest::header::HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    /// The list is the cloud's to grow, so it is read leniently: case and
    /// whitespace do not matter, unknown tokens are passed over, and only a
    /// whole token counts.
    #[test]
    fn a_capability_is_one_whole_token_of_a_comma_separated_list_read_leniently() {
        for stated in [
            &["collect"][..],
            &["Collect"],
            &["COLLECT"],
            &["  collect  "],
            &["batch, collect"],
            &["collect,a_capability_from_a_later_cloud"],
            &["batch ,\tcollect , other"],
            &["batch", "collect"],
        ] {
            assert!(states(&headers(stated), COLLECT), "{stated:?}");
        }
        for unstated in [
            &[][..],
            &[""],
            &[","],
            &["collector"],
            &["no-collect"],
            &["collect=1"],
            &["batch, other"],
            &["col lect"],
        ] {
            assert!(!states(&headers(unstated), COLLECT), "{unstated:?}");
        }
    }

    /// A header of another name states nothing, whatever it holds, and
    /// neither does a value that is not text.
    #[test]
    fn only_the_capabilities_header_states_a_capability() {
        let mut map = reqwest::header::HeaderMap::new();
        map.insert(
            "X-Carrick-Collect",
            reqwest::header::HeaderValue::from_static("collect"),
        );
        assert!(!states(&map, COLLECT));

        let mut opaque = reqwest::header::HeaderMap::new();
        opaque.insert(
            CAPABILITIES_HEADER,
            reqwest::header::HeaderValue::from_bytes(b"collect\xff").unwrap(),
        );
        assert!(!states(&opaque, COLLECT));
    }

    /// Never heard is no. Heard without the token is no. Once stated it
    /// stays stated, each endpoint is its own, and a run starts from nothing.
    #[test]
    fn collect_is_stated_per_endpoint_and_forgotten_as_a_run_opens() {
        let heard = CloudCapabilities::new();
        let files = "https://api.example/analyze-file";
        assert!(!heard.collects(files));

        heard.heard(files, false);
        assert!(!heard.collects(files));

        heard.heard(files, true);
        assert!(heard.collects(files));
        heard.heard(files, false);
        assert!(heard.collects(files), "an answer without it took it back");

        assert!(!heard.collects("https://api.example/generate-intent"));
        assert!(!heard.collects("https://other.example/analyze-file"));

        heard.reset();
        assert!(!heard.collects(files));
    }

    /// The log says which it was, in a line short enough to read.
    #[test]
    fn the_statement_line_says_what_follows_from_it() {
        let stated = statement_line("https://api.example/analyze-file", true);
        let unstated = statement_line("https://api.example/analyze-file", false);
        assert!(stated.contains("states `collect`"), "{stated}");
        assert!(unstated.contains("has not stated `collect`"), "{unstated}");
        for line in [stated, unstated] {
            assert!(line.split_whitespace().count() <= 25, "{line}");
        }
    }
}
