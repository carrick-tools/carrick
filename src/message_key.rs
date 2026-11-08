//! The row key of a message (pub/sub or socket) row, decided once
//! (carrick#3498).
//!
//! Two writers state message rows: the library-claim writer
//! ([`crate::library_claims`]) and the site-row writer ([`crate::site_rows`]).
//! Both ask this module for the three parts of a key that are not the name:
//!
//! - **Name scope** ([`name_scope`]). Whether a name crosses services. An
//!   in-process bus stays in its service (ruling D4, carrick#1564). Else a
//!   scope the library's claim states wins. Else the kind the model named
//!   (`message` is `global`, `event` is `service`). Else no scope: a broker's
//!   topic or a socket's event that nothing scopes is the cross-service
//!   contract Carrick exists to find, so it crosses services, as every row
//!   written before scopes paired did (carrick#3531). The namespace is the
//!   claim's, else the caller's. Pairing reads it through
//!   `carrick_match::names_pair` (`crates/carrick-match/src/name_scope.rs`).
//! - **Socket direction** ([`socket_direction`]). A send on a client's socket
//!   goes client to server, a send on a server's the other way, and a
//!   handler takes the opposite of a send. A library that serves both ends
//!   (`both`) gives [`SocketDirection::Unknown`]: an unknown-direction
//!   listener and emitter meet on one key (`crate::operation`,
//!   [`SocketDirection`]). A side nothing states gives no direction; the
//!   caller decides what that means.
//! - **Lead side** ([`lead_side`]). Which side of the index a lead's row
//!   would stand on: a publish and a socket send are call rows, a subscriber
//!   and a socket listener endpoint rows, the reverse of an HTTP route.

use crate::operation::SocketDirection;
use crate::site_rows::RowSide;

/// The scope a message row is written with, `None` for no scope (the name
/// crosses services). `claimed` is the scope the library's claim states,
/// `kind_scope` the one the model's kind names (`global` or `service`),
/// `namespace` the namespace the caller read.
pub fn name_scope(
    in_process: bool,
    claimed: Option<carrick_match::NameScope>,
    kind_scope: Option<&str>,
    namespace: Option<String>,
) -> Option<carrick_match::NameScope> {
    let claimed_namespace = claimed.as_ref().and_then(|scope| scope.namespace.clone());
    let namespace = claimed_namespace.or(namespace);
    let scope = if in_process {
        carrick_match::SERVICE_SCOPE.to_string()
    } else if let Some(claimed) = claimed {
        claimed.scope
    } else {
        kind_scope?.to_string()
    };
    Some(carrick_match::NameScope { scope, namespace })
}

/// The direction of a socket call or handler on a `side` end (`client`,
/// `server` or `both`); `None` for a side nothing states.
pub fn socket_direction(side: Option<&str>, row: RowSide) -> Option<SocketDirection> {
    match (side?, row) {
        ("server", RowSide::Call) | ("client", RowSide::Endpoint) => {
            Some(SocketDirection::ServerToClient)
        }
        ("client", RowSide::Call) | ("server", RowSide::Endpoint) => {
            Some(SocketDirection::ClientToServer)
        }
        ("both", _) => Some(SocketDirection::Unknown),
        _ => None,
    }
}

/// The side of the index a lead of protocol `kind` and `role` (`producer` or
/// `consumer`) would be a row on; `None` for a lead that says no role. A
/// message's producer is a call row, an HTTP route's producer an endpoint.
pub fn lead_side(kind: Option<&str>, role: Option<&str>) -> Option<RowSide> {
    let message = matches!(kind, Some("pubsub" | "socket"));
    match (role?, message) {
        ("consumer", true) | ("producer", false) => Some(RowSide::Endpoint),
        ("producer", true) | ("consumer", false) => Some(RowSide::Call),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(scope: &str, namespace: Option<&str>) -> carrick_match::NameScope {
        carrick_match::NameScope {
            scope: scope.to_string(),
            namespace: namespace.map(str::to_string),
        }
    }

    #[test]
    fn a_claim_s_scope_wins_over_the_kind() {
        let got = name_scope(
            false,
            Some(scope("global", Some("q"))),
            Some("service"),
            None,
        );
        assert_eq!(got, Some(scope("global", Some("q"))));
    }

    #[test]
    fn an_in_process_bus_is_service_whatever_is_stated() {
        let got = name_scope(true, Some(scope("global", Some("q"))), Some("global"), None);
        assert_eq!(got, Some(scope("service", Some("q"))));
        assert_eq!(
            name_scope(true, None, None, None),
            Some(scope("service", None))
        );
    }

    #[test]
    fn a_silent_claim_reads_the_kind_then_no_scope() {
        assert_eq!(
            name_scope(false, None, Some("global"), None),
            Some(scope("global", None))
        );
        assert_eq!(
            name_scope(false, None, Some("service"), None),
            Some(scope("service", None))
        );
        assert_eq!(name_scope(false, None, None, Some("ns".into())), None);
    }

    #[test]
    fn both_sides_give_an_unknown_direction_and_none_gives_none() {
        for row in [RowSide::Call, RowSide::Endpoint] {
            assert_eq!(
                socket_direction(Some("both"), row),
                Some(SocketDirection::Unknown)
            );
            assert_eq!(socket_direction(None, row), None);
        }
    }

    #[test]
    fn a_socket_lead_flips_its_side_as_a_pubsub_lead_does() {
        for kind in ["pubsub", "socket"] {
            assert_eq!(lead_side(Some(kind), Some("producer")), Some(RowSide::Call));
            assert_eq!(
                lead_side(Some(kind), Some("consumer")),
                Some(RowSide::Endpoint)
            );
        }
        assert_eq!(
            lead_side(Some("http"), Some("consumer")),
            Some(RowSide::Call)
        );
        assert_eq!(
            lead_side(Some("http"), Some("producer")),
            Some(RowSide::Endpoint)
        );
        assert_eq!(lead_side(Some("socket"), None), None);
    }
}
