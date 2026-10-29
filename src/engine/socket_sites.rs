//! Where a service states a socket row (carrick#1670 T4, decision Q5), so
//! that the in-process event-bus rows and the model's pub/sub rows give way
//! to every socket row the scan states, not only the socket pass's.
//!
//! The set holds the socket pass's ops, the library rows of socket kind the
//! service states, and the site rows with a socket key that will stand (a
//! fact or an in-band candidate on no label-only slot: a row below the band
//! becomes a possible call in the site fold and states nothing, so it must
//! not take a bus row's place).
//!
//! Two readers:
//!
//! - [`SocketSites::yield_bus`] drops every event-bus op at a socket site's
//!   file, line and event, before [`super::classify_in_process_pubsub`] reads
//!   the bus ops as the other side of a topic and before the append. The bus
//!   pass's rows carry no source, so the site fold keeps one and drops a
//!   socket site row at the same line (`site_rows::fold_rows`); without this
//!   a socket call read only through a decided site became a pub/sub row.
//! - [`SocketSites::has_twin`] is the same-file twin fold of the model's
//!   pub/sub rows ([`super::append_pubsub_operations`],
//!   [`super::append_pubsub_manifest_entries`]). The file-analyzer sometimes
//!   reads one `socket.emit("x", …)` or `socket.on("x", …)` as both a socket
//!   event and a pub/sub op; the socket row is the contract, so a pub/sub row
//!   in the same file on the same name is dropped, or the call is indexed
//!   twice. Keyed by file and name, not line, and on no library or broker
//!   name, so a real broker publish in a file with no socket row on that name
//!   is untouched. An in-process bus op folds no model row: it is on the same
//!   channel, and the model's row is the richer (it carries a payload
//!   anchor), so that fold runs the other way, in
//!   [`super::append_event_bus_operations`] (carrick#676).
//!
//! Files are compared relative to the repo root whichever form a reader
//! holds ([`super::protocol_relative`], carrick#1874).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::ProtocolExtractions;
use crate::config::Config;
use crate::library_claims::LibraryRowKind;

/// A service's socket sites (module docs).
#[derive(Debug, Default)]
pub(super) struct SocketSites {
    repo_root: PathBuf,
    /// `(file, line, event)`, the file relative to the repo root.
    sites: HashSet<(PathBuf, u32, String)>,
    /// File → every event a socket row names in it.
    events: HashMap<PathBuf, HashSet<String>>,
    /// The bus ops [`Self::yield_bus`] dropped, for the bus debug line.
    pub(super) yielded: usize,
}

impl SocketSites {
    /// The socket sites of `extractions`: the socket pass's ops, the library
    /// socket rows `service` states, and its standing socket site rows.
    pub(super) fn of(extractions: &ProtocolExtractions, repo_path: &str, service: &Config) -> Self {
        let mut sites = SocketSites {
            repo_root: super::normalize_protocol_file(Path::new(repo_path)),
            ..SocketSites::default()
        };
        let sockets = &extractions.sockets;
        for op in sockets.listeners.iter().chain(&sockets.emitters) {
            if let Some(event) = op.key.socket_event() {
                sites.insert(&op.file_path, op.line, event);
            }
        }
        let stated = super::stated_library_rows(&extractions.library, repo_path, service);
        for row in &stated.rows {
            if matches!(row.kind, LibraryRowKind::Socket { .. }) {
                sites.insert(&row.file, row.line, &row.name);
            }
        }
        for row in &extractions.site.rows {
            if let Some(event) = row.key.socket_event()
                && row.stands()
            {
                sites.insert(Path::new(&row.file), row.line, event);
            }
        }
        sites
    }

    fn insert(&mut self, file: &Path, line: u32, event: &str) {
        let file = super::protocol_relative(&self.repo_root, file);
        self.events
            .entry(file.clone())
            .or_default()
            .insert(event.to_string());
        self.sites.insert((file, line, event.to_string()));
    }

    /// Whether a socket row stands at `file`, `line` for `event`.
    fn holds(&self, file: &Path, line: u32, event: &str) -> bool {
        self.sites.contains(&(
            super::protocol_relative(&self.repo_root, file),
            line,
            event.to_string(),
        ))
    }

    /// Whether a socket row in `file` names `topic`: a pub/sub row there for
    /// the same name is the model reading the socket call again.
    pub(super) fn has_twin(&self, file: &Path, topic: &str) -> bool {
        self.events
            .get(&super::protocol_relative(&self.repo_root, file))
            .is_some_and(|events| events.contains(topic))
    }

    /// Drop every bus op at a socket site, subscribers and publishers alike.
    /// Returns how many went, and counts them in [`Self::yielded`].
    pub(super) fn yield_bus(&mut self, bus: &mut crate::event_emitter::BusExtraction) -> usize {
        let before = bus.subscribers.len() + bus.publishers.len();
        let stays =
            |op: &crate::event_emitter::BusOp| !self.holds(&op.file_path, op.line, &op.event);
        bus.subscribers.retain(stays);
        bus.publishers.retain(stays);
        let gone = before - bus.subscribers.len() - bus.publishers.len();
        self.yielded += gone;
        gone
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_emitter::{BusExtraction, BusOp};
    use crate::operation::OperationKey;
    use crate::site_rows::{InferenceBy, SiteInference, SiteRow, SiteRows};

    const FILE: &str = "src/server-socket.ts";

    fn bus_op(file: &str, line: u32, event: &str) -> BusOp {
        BusOp {
            key: OperationKey::pubsub(event.to_string()),
            event: event.to_string(),
            file_path: PathBuf::from(file),
            line,
        }
    }

    /// The site rows' socket row (`chat` at `src/server-socket.ts:7`), as a
    /// fact or as an answer at `confidence`.
    fn site_row(confidence: Option<f64>, needs_reader: bool) -> SiteRow {
        let mut row = crate::site_rows::tests::a_socket_fact_row();
        assert_eq!((row.file.as_str(), row.line), (FILE, 7));
        if let Some(confidence) = confidence {
            row.inference = Some(SiteInference {
                by: InferenceBy::SiteQuestion,
                question_set: None,
                confidence,
                orders_agree: true,
            });
            row.in_band = confidence >= crate::site_rows::BAND;
        }
        row.needs_reader = needs_reader;
        row
    }

    fn sites_of(row: SiteRow) -> SocketSites {
        let extractions = ProtocolExtractions {
            site: SiteRows {
                enabled: true,
                rows: vec![row],
                ..SiteRows::default()
            },
            ..ProtocolExtractions::default()
        };
        SocketSites::of(&extractions, "/repo", &Config::default())
    }

    /// The bus pass's ops around the site row, with the paths a full scan
    /// discovers.
    fn bus() -> BusExtraction {
        BusExtraction {
            subscribers: vec![bus_op("./src/server-socket.ts", 7, "chat")],
            publishers: vec![
                bus_op("/repo/src/server-socket.ts", 7, "chat"),
                // Another event on the same line.
                bus_op("/repo/src/server-socket.ts", 7, "typing"),
                // The same event on another line.
                bus_op("/repo/src/server-socket.ts", 9, "chat"),
            ],
        }
    }

    fn left(bus: &BusExtraction) -> Vec<(u32, &str)> {
        bus.subscribers
            .iter()
            .chain(&bus.publishers)
            .map(|op| (op.line, op.event.as_str()))
            .collect()
    }

    #[test]
    fn a_bus_op_at_a_standing_socket_site_row_goes() {
        let mut bus = bus();
        assert_eq!(sites_of(site_row(None, false)).yield_bus(&mut bus), 2);
        assert_eq!(left(&bus), vec![(7, "typing"), (9, "chat")]);
    }

    #[test]
    fn an_answer_in_the_band_takes_the_bus_row_s_place() {
        let mut bus = bus();
        assert_eq!(sites_of(site_row(Some(1.0), false)).yield_bus(&mut bus), 2);
    }

    #[test]
    fn a_bus_op_at_a_below_band_socket_site_row_stays() {
        let mut bus = bus();
        assert_eq!(sites_of(site_row(Some(0.1), false)).yield_bus(&mut bus), 0);
        assert_eq!(left(&bus).len(), 4);
    }

    #[test]
    fn a_bus_op_at_a_site_row_that_needs_a_reader_stays() {
        let mut bus = bus();
        assert_eq!(sites_of(site_row(None, true)).yield_bus(&mut bus), 0);
    }

    #[test]
    fn a_model_row_in_the_file_of_a_socket_site_row_is_its_twin() {
        let sites = sites_of(site_row(None, false));
        assert!(sites.has_twin(Path::new(FILE), "chat"));
        assert!(sites.has_twin(Path::new("/repo/src/server-socket.ts"), "chat"));
        assert!(!sites.has_twin(Path::new(FILE), "typing"));
        assert!(!sites.has_twin(Path::new("src/other.ts"), "chat"));
    }
}
