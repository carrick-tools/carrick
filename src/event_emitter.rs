//! Deterministic in-process event-bus contract extraction.
//!
//! A service that publishes work through an in-process emitter
//! (`bus.emit("orderPlaced", …)`) and subscribes to it somewhere else
//! (`bus.on("orderPlaced", handler)`) has a contract with two sides and a name,
//! exactly like a broker topic — the only difference is that the message never
//! leaves the process. The index holds it on the same channel a broker topic
//! uses, `OperationKey::pubsub(<event>)`: a subscription registers the handler
//! and is the contract PRODUCER, an emission sends and is the CONSUMER.
//!
//! This is the one shape the rest of the pipeline could not see. The
//! file-analyzer reports pub/sub operations for brokers, and the socket pass
//! (`crate::socket_io`) covers Socket.IO transport, but an emitter held on a
//! plain object field belongs to neither, so a question like "what subscribes
//! to this notification" had no row to answer with (carrick#676).
//!
//! The pass is structural throughout: it matches the *shape* of the call
//! (`<anything>.on|once|addListener("literal", …)` and
//! `<anything>.emit("literal", …)`), never a library or class name, so any
//! object exposing the EventEmitter protocol resolves and no package needs to
//! be recognised. Three rules keep that from becoming noise:
//!
//! - **Literal event names only.** A computed name has no identity to key on.
//! - **The runtime's own vocabulary is excluded** (see [`RUNTIME_EVENTS`]).
//!   These are events user code subscribes to but never emits — the runtime
//!   emits them — so they are not contracts between two pieces of this
//!   codebase, and indexing them would put a producer row on a key as generic
//!   as `error` or `data`, which any other repo's row would then match.
//! - **A site the socket pass already recorded is left alone.** Socket ops are
//!   the modeled transport contract and live on their own key; the same span
//!   must not be indexed twice on two channels.
//!
//! What that accepts, stated plainly. An event whose name is a literal but
//! whose emitter is a third-party object with its own vocabulary (a redis
//! client's `pmessage`, a queue's `stalled`) produces a subscriber row with no
//! publisher anywhere — an orphaned producer, not a wrong one. And identity is
//! the event name alone, as it is for every pub/sub row, so two services
//! naming an event the same thing share a key whether or not they share a bus;
//! that exposure is the one broker topics already carry.

use crate::operation::OperationKey;
use crate::parser::parse_file;
use crate::socket_io::{RESERVED_EVENTS, SocketExtraction};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use swc_common::errors::{ColorConfig, Handler};
use swc_common::{GLOBALS, Globals, SourceMap, Span, Spanned, sync::Lrc};
use swc_ecma_ast::{Callee, Expr, ExprOrSpread, Lit, MemberExpr, OptChainBase, OptChainExpr};
use swc_ecma_visit::{Visit, VisitWith};
use tracing::debug;

/// One side of an in-process event contract, with its source location.
#[derive(Debug, Clone)]
pub struct BusOp {
    /// Always `OperationKey::pubsub(event)` — an in-process bus is pub/sub with
    /// a shorter wire.
    pub key: OperationKey,
    /// The literal event name, kept alongside the key so the twin fold can read
    /// it without re-parsing the key.
    pub event: String,
    pub file_path: PathBuf,
    pub line: u32,
}

#[derive(Debug, Clone, Default)]
pub struct BusExtraction {
    /// `.on` / `.once` / `.addListener` — the handler side, contract producers.
    pub subscribers: Vec<BusOp>,
    /// `.emit` — the sending side, contract consumers.
    pub publishers: Vec<BusOp>,
}

impl BusExtraction {
    pub fn is_empty(&self) -> bool {
        self.subscribers.is_empty() && self.publishers.is_empty()
    }

    fn merge(&mut self, other: BusExtraction) {
        self.subscribers.extend(other.subscribers);
        self.publishers.extend(other.publishers);
    }
}

/// Methods that register a handler. `prependListener` and its `once` variant
/// are the same registration with a different queue position, so they count.
const SUBSCRIBE_METHODS: &[&str] = &[
    "on",
    "once",
    "addListener",
    "prependListener",
    "prependOnceListener",
];

/// The method that sends.
const PUBLISH_METHOD: &str = "emit";

/// The runtime's own reserved vocabulary: events a Node process, stream,
/// socket, or child process emits at code that subscribes to them. User code is
/// on one side of these only, so they are lifecycle, not a contract between two
/// parts of a codebase — and their names are generic enough (`error`, `data`,
/// `close`) that a row on one would match any unrelated row sharing the name.
///
/// This is an exclusion vocabulary, not a library list: nothing here names a
/// package, and a package's own event names are not enumerated anywhere. The
/// Socket.IO lifecycle names are unioned in from
/// [`crate::socket_io::RESERVED_EVENTS`], because the socket pass DECLINES
/// those sites (they are reserved there too) and so leaves no claim on the span
/// for this pass to see.
const RUNTIME_EVENTS: &[&str] = &[
    // Process lifecycle and signals.
    "SIGBREAK",
    "SIGHUP",
    "SIGINT",
    "SIGQUIT",
    "SIGTERM",
    "SIGUSR1",
    "SIGUSR2",
    "SIGWINCH",
    "beforeExit",
    "exit",
    "rejectionHandled",
    "uncaughtException",
    "unhandledRejection",
    "warning",
    // Streams, sockets, servers.
    "aborted",
    "clientError",
    "continue",
    "data",
    "drain",
    "end",
    "finish",
    "listening",
    "lookup",
    "open",
    "pause",
    "pipe",
    "readable",
    "ready",
    "request",
    "response",
    "resume",
    "secureConnection",
    "timeout",
    "unpipe",
    "upgrade",
    // Child processes and workers.
    "close",
    "message",
    "messageerror",
    "online",
    "spawn",
];

/// Whether an event name is the runtime's rather than the codebase's.
fn is_reserved(event: &str) -> bool {
    RUNTIME_EVENTS.contains(&event) || RESERVED_EVENTS.contains(&event)
}

/// Extract in-process event-bus operations from the service's TS/JS files.
///
/// `sockets` is the deterministic Socket.IO extraction for the SAME file set,
/// already run: every span it recorded is claimed and skipped here, so one call
/// site never produces both a `socket|…` and a `pubsub|…` row.
pub fn scan_files(service_files: &[PathBuf], sockets: &SocketExtraction) -> BusExtraction {
    let claimed = claimed_spans(sockets);
    let mut extraction = BusExtraction::default();
    for file in service_files {
        let is_script = file
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| matches!(ext, "ts" | "tsx" | "js" | "jsx"));
        if !is_script {
            continue;
        }
        extraction.merge(extract_from_ts_file(file, &claimed));
    }
    debug!(
        subscribers = extraction.subscribers.len(),
        publishers = extraction.publishers.len(),
        "In-process event bus extraction complete"
    );
    extraction
}

/// Spans the socket pass already recorded, as (file, line, event). Keyed on the
/// event as well as the span so two calls sharing a line — `bus.on("a", () =>
/// socket.emit("b", x))` — are told apart.
fn claimed_spans(sockets: &SocketExtraction) -> HashSet<(PathBuf, u32, String)> {
    sockets
        .listeners
        .iter()
        .chain(sockets.emitters.iter())
        .filter_map(|op| {
            op.key
                .socket_event()
                .map(|event| (op.file_path.clone(), op.line, event.to_string()))
        })
        .collect()
}

fn extract_from_ts_file(
    file_path: &Path,
    claimed: &HashSet<(PathBuf, u32, String)>,
) -> BusExtraction {
    let cm: Lrc<SourceMap> = Default::default();
    let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));

    let globals = Globals::new();
    GLOBALS.set(&globals, || {
        let Some(module) = parse_file(file_path, &cm, &handler) else {
            return BusExtraction::default();
        };
        let mut collector = BusCollector {
            cm: cm.clone(),
            file_path,
            claimed,
            extraction: BusExtraction::default(),
        };
        module.visit_with(&mut collector);
        collector.extraction
    })
}

struct BusCollector<'a> {
    cm: Lrc<SourceMap>,
    file_path: &'a Path,
    claimed: &'a HashSet<(PathBuf, u32, String)>,
    extraction: BusExtraction,
}

impl Visit for BusCollector<'_> {
    fn visit_call_expr(&mut self, node: &swc_ecma_ast::CallExpr) {
        if let Callee::Expr(callee) = &node.callee
            && let Expr::Member(member) = &**callee
        {
            self.record_member_call(member, &node.args, node.span());
        }
        node.visit_children_with(self);
    }

    fn visit_opt_chain_expr(&mut self, node: &OptChainExpr) {
        // `bus?.on("x", …)` is an optional call, not a `CallExpr`, so it needs
        // its own arm or the op is silently lost.
        if let OptChainBase::Call(call) = &*node.base {
            match &*call.callee {
                Expr::Member(member) => self.record_member_call(member, &call.args, node.span()),
                Expr::OptChain(inner) => {
                    if let OptChainBase::Member(member) = &*inner.base {
                        self.record_member_call(member, &call.args, node.span());
                    }
                }
                _ => {}
            }
        }
        node.visit_children_with(self);
    }
}

impl BusCollector<'_> {
    /// Record the bus op a `<receiver>.<method>("event", …)` call site carries,
    /// if any. The receiver is deliberately unconstrained: what makes this an
    /// event bus is the protocol it answers, not what it was built from.
    fn record_member_call(&mut self, member: &MemberExpr, args: &[ExprOrSpread], span: Span) {
        let Some(prop) = member.prop.as_ident() else {
            return;
        };
        let method = prop.sym.as_ref();
        let is_subscribe = SUBSCRIBE_METHODS.contains(&method);
        let is_publish = method == PUBLISH_METHOD;
        if !is_subscribe && !is_publish {
            return;
        }
        // A registration takes a handler; an emission usually carries a
        // payload. Neither is required to have one, but a literal event name
        // is: without it there is no key.
        let Some(first) = args.first() else {
            return;
        };
        let Expr::Lit(Lit::Str(event)) = &*first.expr else {
            return;
        };
        let event = event.value.to_string();
        if is_reserved(&event) {
            return;
        }
        let line = self.cm.lookup_char_pos(span.lo).line as u32;
        if self
            .claimed
            .contains(&(self.file_path.to_path_buf(), line, event.clone()))
        {
            debug!(
                event = %event,
                file = %self.file_path.display(),
                line,
                "event-bus site already recorded as a socket op; leaving it there"
            );
            return;
        }
        let op = BusOp {
            key: OperationKey::pubsub(event.clone()),
            event,
            file_path: self.file_path.to_path_buf(),
            line,
        };
        if is_subscribe {
            self.extraction.subscribers.push(op);
        } else {
            self.extraction.publishers.push(op);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Scan one file's source and return what the pass found, with no socket
    /// ops claimed.
    fn scan(source: &str) -> BusExtraction {
        scan_with_sockets(source, &SocketExtraction::default()).1
    }

    /// Scan one file's source, running the socket pass over it first so its
    /// claims are real rather than hand-built.
    fn scan_with_sockets(source: &str, sockets: &SocketExtraction) -> (PathBuf, BusExtraction) {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("bus.ts");
        fs::write(&file, source).unwrap();
        let files = vec![file.clone()];
        let extraction = scan_files(&files, sockets);
        // Keep the directory alive until after the scan.
        drop(dir);
        (file, extraction)
    }

    fn events(ops: &[BusOp]) -> Vec<&str> {
        ops.iter().map(|op| op.event.as_str()).collect()
    }

    /// The registration methods all produce a producer row, on the pub/sub key.
    #[test]
    fn every_registration_form_is_a_subscriber() {
        let found = scan(
            r#"
            bus.on("orderPlaced", handleOrder);
            bus.once("orderShipped", handleShipped);
            bus.addListener("orderRefunded", handleRefund);
            bus.prependListener("orderArchived", handleArchive);
            "#,
        );
        assert_eq!(
            events(&found.subscribers),
            vec![
                "orderPlaced",
                "orderShipped",
                "orderRefunded",
                "orderArchived"
            ]
        );
        assert!(found.publishers.is_empty());
        assert_eq!(
            found.subscribers[0].key.canonical(),
            "pubsub|orderPlaced",
            "an in-process bus row lives on the pub/sub channel"
        );
    }

    /// `.emit` is the consumer side, and carries the line it was written on.
    #[test]
    fn an_emission_is_a_publisher_at_its_own_line() {
        let found = scan(
            r#"
            function ship() {
              bus.emit("orderShipped", { id });
            }
            "#,
        );
        assert!(found.subscribers.is_empty());
        assert_eq!(events(&found.publishers), vec!["orderShipped"]);
        assert_eq!(found.publishers[0].line, 3);
    }

    /// The motivating shape (carrick#676): the bus is a field on some other
    /// object, so the receiver is a chain rather than a bare name.
    #[test]
    fn a_bus_reached_through_a_member_chain_resolves() {
        let found = scan(
            r#"
            engine.eventBus.on("workerNotification", onNotification);
            this.deps.bus?.emit("workerNotification", payload);
            "#,
        );
        assert_eq!(events(&found.subscribers), vec!["workerNotification"]);
        assert_eq!(events(&found.publishers), vec!["workerNotification"]);
    }

    /// No literal, no key: a computed event name has no identity to index.
    #[test]
    fn a_computed_event_name_is_not_indexed() {
        let found = scan(
            r#"
            bus.on(eventName, handler);
            bus.emit(`${prefix}.created`, payload);
            bus.on();
            "#,
        );
        assert!(found.is_empty(), "{found:?}");
    }

    /// The runtime's own vocabulary is lifecycle, not a contract.
    #[test]
    fn runtime_lifecycle_events_are_not_contracts() {
        let found = scan(
            r#"
            process.on("SIGTERM", shutdown);
            stream.on("data", chunk => buffer.push(chunk));
            stream.on("error", fail);
            child.on("close", done);
            client.on("disconnect", reconnect);
            "#,
        );
        assert!(found.is_empty(), "{found:?}");
    }

    /// A method that is not the emitter protocol is not a bus call, however
    /// literal its first argument.
    #[test]
    fn other_methods_are_not_bus_calls() {
        let found = scan(
            r#"
            query.join("orders", "orders.id");
            router.get("/orders", listOrders);
            el.addEventListener("click", onClick);
            "#,
        );
        assert!(found.is_empty(), "{found:?}");
    }

    /// A span the socket pass recorded stays on the socket channel: one call
    /// site, one row. The socket extraction here is the real one, produced by
    /// running that pass over the same file.
    #[test]
    fn a_socket_claimed_span_is_not_indexed_twice() {
        let source = r#"
            import { io } from "socket.io-client";
            const client = io("https://example.test");
            client.on("orderPlaced", handleOrder);
            bus.on("orderArchived", handleArchive);
        "#;
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("client.ts");
        fs::write(&file, source).unwrap();
        let files = vec![file.clone()];

        let sockets = crate::socket_io::scan_files(&files);
        assert_eq!(
            sockets.listeners.len(),
            1,
            "fixture must produce the socket op this test folds against"
        );

        let found = scan_files(&files, &sockets);
        assert_eq!(
            events(&found.subscribers),
            vec!["orderArchived"],
            "the socket-claimed span must not also become a pub/sub row"
        );
    }
}
