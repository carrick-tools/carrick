//! Protocol-tagged operation identity.
//!
//! Every producer (endpoint) and consumer (outbound call) is keyed by an
//! [`OperationKey`]. The protocol tag travels with the key so operations from
//! different protocols can never collide in matching, type-manifest aliases,
//! or the cloud index. HTTP-only code paths (mount-graph matching, REST
//! manifest building) dispatch through [`OperationKey::as_http`] and skip
//! other protocols; each protocol gets its own matcher.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Protocol families Carrick recognizes. Used to tag operation keys and to
/// route the LLM pipeline: SWC candidates carry a protocol, and each
/// protocol with non-deterministic evidence gets its own guidance and
/// analyze-file prompt instead of diluting one prompt across all of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Http,
    /// Extracted deterministically (SDL + documents); never LLM-routed.
    Graphql,
    /// `new WebSocket(...)` / `new EventSource(...)` call sites. Tagged by
    /// the scanner today so they stop reaching the HTTP prompt; the
    /// socket-specific prompt arrives with the Phase 2 work.
    Websocket,
    /// Topic-keyed publish/subscribe (Redis pub/sub today; Kafka/NATS are
    /// future adapters under the same family). A subscriber is the producer
    /// (endpoint); a publisher is the consumer (call). Identity is the topic
    /// alone — the broker is diagnostic, not part of the key.
    Pubsub,
}

/// Per-pair type-compatibility verdict, the three-way result of the type
/// sidecar's check. Distinct from `Option<bool>` (`type_compatible`): that
/// collapses `Unverifiable` and "never evaluated" both to `None`, which is
/// exactly the distinction the PR-comment / terminal "Verified" buckets and
/// the MCP `check_compatibility` tool need to keep honest (#260, cloud#268).
/// Absence of a verdict (edge never evaluated) is represented by `Option::None`
/// around this enum, never by a variant here.
///
/// Wire strings are lowercase (`"compatible"` / `"incompatible"` /
/// `"unverifiable"`); the cloud PR-comment renderer keys the "Type-checked"
/// and "Types not verifiable" buckets on the first and third, and treats any
/// other value (including absent) as "Types not compared".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeVerdict {
    Compatible,
    Incompatible,
    Unverifiable,
}

impl TypeVerdict {
    /// Collapse two per-consumer verdicts for the same producer endpoint into
    /// one per-endpoint verdict, honesty-first: a row is only `Compatible`
    /// when EVERY evaluated consumer pair is compatible; any `Unverifiable`
    /// pair degrades the row to `Unverifiable`; any `Incompatible` pair marks
    /// it `Incompatible` (already surfaced as a loud finding elsewhere).
    /// Precedence: `Incompatible` > `Unverifiable` > `Compatible`.
    pub fn combine(self, other: Self) -> Self {
        use TypeVerdict::*;
        match (self, other) {
            (Incompatible, _) | (_, Incompatible) => Incompatible,
            (Unverifiable, _) | (_, Unverifiable) => Unverifiable,
            (Compatible, Compatible) => Compatible,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphqlOperationKind {
    Query,
    Mutation,
    Subscription,
}

/// Semantic classification of an outbound call, orthogonal to `Protocol` (the
/// wire format). Assigned by the file-analyzer LLM. Only `InternalHttp` is meant
/// to feed cross-service compatibility matching; `ExternalHttp` / `Sdk` are
/// excluded from compat and earmarked for a future dependency view — though today
/// many SDK/external targets are still dropped upstream by
/// `analyzer::is_valid_route_shape` (non-route shapes: `||`, parens, whitespace)
/// before they reach the graph, so that retention is not yet complete. `Unresolved`
/// (and an absent label) is excluded from matching. The gating that acts on this
/// lands in a later stage; today the field is captured and carried only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallKind {
    InternalHttp,
    ExternalHttp,
    Sdk,
    Unresolved,
}

impl CallKind {
    /// Parse a model-emitted kind string leniently (case-insensitive; `-`/space
    /// separators tolerated). Returns `None` for anything off-enum so one junk
    /// value can't fail the whole file's deserialization (mirrors EmissionStyle).
    pub fn parse_lenient(value: &str) -> Option<Self> {
        match value
            .trim()
            .to_ascii_lowercase()
            .replace(['-', ' '], "_")
            .as_str()
        {
            "internal_http" => Some(CallKind::InternalHttp),
            "external_http" => Some(CallKind::ExternalHttp),
            "sdk" => Some(CallKind::Sdk),
            "unresolved" => Some(CallKind::Unresolved),
            _ => None,
        }
    }
}

/// Where a producer endpoint's evidence comes from: a real runtime route, or
/// a handler registered in a mock/test tree (e.g. a mock-service-worker style
/// `http.get(...)` under `src/mocks/`).
///
/// Classified STRUCTURALLY from the endpoint's source path (directory-name
/// conventions shared across the ecosystem — see
/// [`crate::file_finder::endpoint_provenance`]), never from a
/// framework/package name list. Mock endpoints are still extracted and
/// matched — a mock frequently encodes the org's canonical contract, so
/// mismatches against it are real — but the tag travels through matching into
/// findings and the report so a consumer mismatch whose producer shape comes
/// from a mock can be presented with the right amount of trust.
///
/// `Ord` is deliberate: `Route < Mock`, so `.min()` over several candidate
/// producers implements "route-wins" when a real route and a mock share one
/// operation key.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum EndpointProvenance {
    /// A route registration in product source.
    #[default]
    Route,
    /// A handler registered under a mock/test tree (`mocks/`, `__mocks__/`,
    /// test directories, or test-suffixed files).
    Mock,
}

impl EndpointProvenance {
    pub fn is_mock(&self) -> bool {
        matches!(self, EndpointProvenance::Mock)
    }
}

/// Which side of a pub/sub topic an operation sits on. A subscriber registers a
/// handler for a topic and is the contract producer (endpoint); a publisher
/// sends to a topic and is the contract consumer (call). Assigned by the
/// file-analyzer LLM and parsed leniently (mirrors [`CallKind::parse_lenient`])
/// so one off-enum value can't fail the whole file's deserialization.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PubsubRole {
    Subscriber,
    Publisher,
}

impl PubsubRole {
    /// Parse a model-emitted role string leniently (case-insensitive; `-`/space
    /// separators tolerated). Returns `None` for anything off-enum so one junk
    /// value can't fail the whole file's parse (mirrors [`CallKind::parse_lenient`]).
    pub fn parse_lenient(value: &str) -> Option<Self> {
        match value
            .trim()
            .to_ascii_lowercase()
            .replace(['-', ' '], "_")
            .as_str()
        {
            "subscriber" => Some(PubsubRole::Subscriber),
            "publisher" => Some(PubsubRole::Publisher),
            _ => None,
        }
    }
}

impl GraphqlOperationKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            GraphqlOperationKind::Query => "query",
            GraphqlOperationKind::Mutation => "mutation",
            GraphqlOperationKind::Subscription => "subscription",
        }
    }
}

/// Direction a socket message flows in. Listeners for messages flowing in a
/// direction are producers of that key; emitters sending in that direction
/// are consumers — so a server `socket.on("x")` matches a client
/// `socket.emit("x")` and vice versa.
///
/// A direction exists only where the library distinguishes the two sides by
/// construction: Socket.IO's client factory and `new Server(...)` say which
/// side a socket is on, and the direction is then derived. Libraries that
/// expose one symmetric object to both sides (`ws`, channel clients) say
/// nothing, and their ops carry [`SocketDirection::Unknown`] rather than a
/// guess — an unknown-direction listener and emitter meet on one key, so the
/// two sides of such a contract still match each other (carrick#1281).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SocketDirection {
    ClientToServer,
    ServerToClient,
    /// The library gave no client/server signal. Not a default: it is recorded
    /// only where no rule could derive a direction.
    Unknown,
}

impl SocketDirection {
    pub fn as_str(&self) -> &'static str {
        match self {
            SocketDirection::ClientToServer => "client→server",
            SocketDirection::ServerToClient => "server→client",
            SocketDirection::Unknown => "direction unknown",
        }
    }

    /// ASCII label used in canonical identity strings and report tables.
    pub fn label(&self) -> &'static str {
        match self {
            SocketDirection::ClientToServer => "CLIENT->SERVER",
            SocketDirection::ServerToClient => "SERVER->CLIENT",
            SocketDirection::Unknown => "UNKNOWN",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "protocol", rename_all = "snake_case")]
pub enum OperationKey {
    /// An HTTP operation. For producers, `path` is the mount-resolved route
    /// (e.g. `/api/users/:id`). For consumers, `path` carries the raw call
    /// target (full URL, env-var template, unresolved expression) until URL
    /// normalization runs during matching.
    Http { method: String, path: String },
    /// A GraphQL operation, identified by root operation kind plus top-level
    /// field name (e.g. `query user`). Producers are schema root fields;
    /// consumers are the top-level fields of executable documents.
    Graphql {
        kind: GraphqlOperationKind,
        field: String,
    },
    /// A socket event, identified by event name plus message-flow direction.
    /// The namespace or channel the event travels on is not part of identity
    /// (see the `crate::socket_io` module docs for why it cannot be), and the
    /// direction is [`SocketDirection::Unknown`] for libraries that expose no
    /// client/server distinction.
    Socket {
        event: String,
        direction: SocketDirection,
    },
    /// A publish/subscribe topic, identified by topic name alone. Subscribers
    /// (handler registrations) are producers; publishers (sends) are consumers.
    /// Carries no direction: the role lives on which side (endpoint vs call) the
    /// op sits, and the broker is diagnostic, not part of identity — so a
    /// subscriber and a publisher on the same topic share one key and match.
    Pubsub { topic: String },
}

impl OperationKey {
    pub fn protocol(&self) -> Protocol {
        match self {
            OperationKey::Http { .. } => Protocol::Http,
            OperationKey::Graphql { .. } => Protocol::Graphql,
            OperationKey::Socket { .. } => Protocol::Websocket,
            OperationKey::Pubsub { .. } => Protocol::Pubsub,
        }
    }

    /// Build an HTTP key. The method is stored trimmed and uppercased so one
    /// canonical form flows into aliases, matching, and the index; an empty
    /// method becomes `UNKNOWN` (mirroring `normalize_manifest_method`).
    pub fn http(method: &str, path: impl Into<String>) -> Self {
        let trimmed = method.trim();
        let method = if trimmed.is_empty() {
            "UNKNOWN".to_string()
        } else {
            trimmed.to_uppercase()
        };
        OperationKey::Http {
            method,
            path: path.into(),
        }
    }

    pub fn graphql(kind: GraphqlOperationKind, field: impl Into<String>) -> Self {
        OperationKey::Graphql {
            kind,
            field: field.into(),
        }
    }

    /// Build a pub/sub key. Identity is the topic alone (no direction, no
    /// broker), so a subscriber and a publisher on the same topic produce
    /// equal keys and match exactly.
    pub fn pubsub(topic: impl Into<String>) -> Self {
        OperationKey::Pubsub {
            topic: topic.into(),
        }
    }

    /// `(method, path)` when this is an HTTP operation. HTTP-only code paths
    /// (mount-graph matching, REST manifest building, alias generation)
    /// filter through this — it is the protocol dispatch point.
    pub fn as_http(&self) -> Option<(&str, &str)> {
        match self {
            OperationKey::Http { method, path } => Some((method, path)),
            OperationKey::Graphql { .. }
            | OperationKey::Socket { .. }
            | OperationKey::Pubsub { .. } => None,
        }
    }

    /// The top-level field name when this is a GraphQL operation, else `None`.
    /// GraphQL consumer anchoring joins a `request<T>(DOC)` call site to its op
    /// by this field name.
    pub fn graphql_field(&self) -> Option<&str> {
        match self {
            OperationKey::Graphql { field, .. } => Some(field.as_str()),
            OperationKey::Http { .. }
            | OperationKey::Socket { .. }
            | OperationKey::Pubsub { .. } => None,
        }
    }

    pub fn socket(event: impl Into<String>, direction: SocketDirection) -> Self {
        OperationKey::Socket {
            event: event.into(),
            direction,
        }
    }

    /// The event name when this is a Socket.IO operation, else `None`. Used by
    /// the same-file pub/sub fold to match a spuriously-classified pub/sub op
    /// against the deterministic socket op sharing the same event string.
    pub fn socket_event(&self) -> Option<&str> {
        match self {
            OperationKey::Socket { event, .. } => Some(event.as_str()),
            OperationKey::Http { .. }
            | OperationKey::Graphql { .. }
            | OperationKey::Pubsub { .. } => None,
        }
    }

    /// `(label, name)` pair used by report tables and issue strings: HTTP is
    /// `(method, path)`, GraphQL is `(KIND, field)`, sockets are
    /// `(DIRECTION, event)`.
    pub fn display_labels(&self) -> (String, String) {
        match self {
            OperationKey::Http { method, path } => (method.clone(), path.clone()),
            OperationKey::Graphql { kind, field } => (kind.as_str().to_uppercase(), field.clone()),
            OperationKey::Socket { event, direction } => {
                (direction.label().to_string(), event.clone())
            }
            OperationKey::Pubsub { topic } => ("PUBSUB".to_string(), topic.clone()),
        }
    }

    /// Stable identity string for hashing and dedup keys.
    pub fn canonical(&self) -> String {
        match self {
            OperationKey::Http { method, path } => format!("http|{}|{}", method, path),
            OperationKey::Graphql { kind, field } => {
                format!("graphql|{}|{}", kind.as_str(), field)
            }
            OperationKey::Socket { event, direction } => {
                format!("socket|{}|{}", direction.label(), event)
            }
            // 2-segment: pub/sub identity is the topic alone, no direction.
            OperationKey::Pubsub { topic } => format!("pubsub|{}", topic),
        }
    }
}

impl fmt::Display for OperationKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OperationKey::Http { method, path } => write!(f, "{} {}", method, path),
            OperationKey::Graphql { kind, field } => write!(f, "{} {}", kind.as_str(), field),
            OperationKey::Socket { event, direction } => {
                write!(f, "{} ({})", event, direction.as_str())
            }
            OperationKey::Pubsub { topic } => write!(f, "{} (pub/sub)", topic),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_key_canonicalizes_method() {
        let key = OperationKey::http(" get ", "/users");
        assert_eq!(key.as_http(), Some(("GET", "/users")));
        assert_eq!(key.canonical(), "http|GET|/users");
        assert_eq!(key.to_string(), "GET /users");
    }

    #[test]
    fn empty_method_becomes_unknown() {
        let key = OperationKey::http("", "/users");
        assert_eq!(key.as_http(), Some(("UNKNOWN", "/users")));
    }

    #[test]
    fn serde_round_trip_carries_protocol_tag() {
        let key = OperationKey::http("POST", "/api/orders");
        let json = serde_json::to_string(&key).unwrap();
        assert!(json.contains("\"protocol\":\"http\""), "got {}", json);
        let back: OperationKey = serde_json::from_str(&json).unwrap();
        assert_eq!(back, key);
    }

    #[test]
    fn graphql_key_identity_and_dispatch() {
        let key = OperationKey::graphql(GraphqlOperationKind::Query, "user");
        assert_eq!(key.as_http(), None);
        assert_eq!(key.protocol(), Protocol::Graphql);
        assert_eq!(
            key.display_labels(),
            ("QUERY".to_string(), "user".to_string())
        );
        assert_eq!(key.canonical(), "graphql|query|user");
        assert_eq!(key.to_string(), "query user");

        let json = serde_json::to_string(&key).unwrap();
        assert!(json.contains("\"protocol\":\"graphql\""), "got {}", json);
        let back: OperationKey = serde_json::from_str(&json).unwrap();
        assert_eq!(back, key);
    }

    #[test]
    fn socket_key_identity_and_dispatch() {
        let key = OperationKey::socket("chat:message", SocketDirection::ClientToServer);
        assert_eq!(key.as_http(), None);
        assert_eq!(key.protocol(), Protocol::Websocket);
        assert_eq!(key.canonical(), "socket|CLIENT->SERVER|chat:message");
        assert_eq!(
            key.display_labels(),
            ("CLIENT->SERVER".to_string(), "chat:message".to_string())
        );

        let json = serde_json::to_string(&key).unwrap();
        assert!(json.contains("\"protocol\":\"socket\""), "got {}", json);
        let back: OperationKey = serde_json::from_str(&json).unwrap();
        assert_eq!(back, key);

        let other_direction = OperationKey::socket("chat:message", SocketDirection::ServerToClient);
        assert_ne!(key, other_direction);
    }

    #[test]
    fn pubsub_key_identity_and_dispatch() {
        let key = OperationKey::pubsub("metrics.page_view");
        assert_eq!(key.as_http(), None);
        assert_eq!(key.graphql_field(), None);
        assert_eq!(key.protocol(), Protocol::Pubsub);
        // 2-segment canonical: topic only, no direction or broker.
        assert_eq!(key.canonical(), "pubsub|metrics.page_view");
        assert_eq!(
            key.display_labels(),
            ("PUBSUB".to_string(), "metrics.page_view".to_string())
        );
        assert_eq!(key.to_string(), "metrics.page_view (pub/sub)");

        let json = serde_json::to_string(&key).unwrap();
        assert!(json.contains("\"protocol\":\"pubsub\""), "got {}", json);
        let back: OperationKey = serde_json::from_str(&json).unwrap();
        assert_eq!(back, key);

        // A subscriber and a publisher on the same topic share one key (no
        // direction field), so they match exactly.
        let same_topic = OperationKey::pubsub("metrics.page_view");
        assert_eq!(key, same_topic);
        let other_topic = OperationKey::pubsub("orders.placed");
        assert_ne!(key, other_topic);

        // Lenient role parsing mirrors CallKind.
        assert_eq!(
            PubsubRole::parse_lenient("subscriber"),
            Some(PubsubRole::Subscriber)
        );
        assert_eq!(
            PubsubRole::parse_lenient("Publisher"),
            Some(PubsubRole::Publisher)
        );
        assert_eq!(PubsubRole::parse_lenient("??"), None);
    }

    #[test]
    fn http_and_graphql_keys_never_collide() {
        let http = OperationKey::http("QUERY", "user");
        let gql = OperationKey::graphql(GraphqlOperationKind::Query, "user");
        assert_ne!(http, gql);
        assert_ne!(http.canonical(), gql.canonical());
    }
}
