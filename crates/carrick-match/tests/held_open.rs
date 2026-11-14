//! `Pairing::held_by_open_literals_only` and `leaves_route_literal_open`
//! (carrick#4277): a call that still pairs at head, but only with routes
//! whose literals it leaves to a placeholder.

use carrick_match::{Call, Pairing, Producer, leaves_route_literal_open, pair_winners};

fn route(method: &str, path: &str) -> Producer {
    Producer {
        service: "api".into(),
        protocol: "http".into(),
        method: Some(method.into()),
        path: Some(path.into()),
        ..Producer::default()
    }
}

fn call(method: &str, path: &str) -> Call {
    Call {
        service: "web".into(),
        protocol: "http".into(),
        method: Some(method.into()),
        path: Some(path.into()),
        ..Call::default()
    }
}

fn pair(routes: &[Producer], method: &str, path: &str) -> Pairing {
    pair_winners(routes, &call(method, path))
}

#[test]
fn a_lone_flagged_winner_holds_the_call_open() {
    let pairing = pair(&[route("GET", "/jobs/stats")], "GET", "/jobs/:id");
    assert!(pairing.held_by_open_literals_only());
}

#[test]
fn a_winner_that_leaves_no_literal_open_does_not() {
    let pairing = pair(&[route("GET", "/jobs/:id")], "GET", "/jobs/:id");
    assert!(!pairing.held_by_open_literals_only());
}

#[test]
fn an_exact_key_answer_does_not() {
    let producer = Producer {
        service: "api".into(),
        protocol: "pubsub".into(),
        name: Some("orders".into()),
        ..Producer::default()
    };
    let pairing = pair_winners(
        &[producer],
        &Call {
            service: "web".into(),
            protocol: "pubsub".into(),
            name: Some("orders".into()),
            ..Call::default()
        },
    );
    assert!(!pairing.held_by_open_literals_only());
}

#[test]
fn a_bare_catch_all_does_not() {
    let pairing = pair(&[route("GET", "/*")], "GET", "/jobs/:id");
    assert!(!pairing.held_by_open_literals_only());
}

#[test]
fn the_floor_shape_does() {
    let pairing = pair(&[route("GET", "/a/b/c")], "GET", "/:p/:q/c");
    assert!(pairing.winners.is_empty());
    assert!(pairing.held_by_open_literals_only());
}

#[test]
fn the_counter_is_the_pools() {
    assert!(leaves_route_literal_open("/jobs/stats", "/jobs/:id"));
    assert!(!leaves_route_literal_open("/jobs/:id", "/jobs/:id"));
    assert!(!leaves_route_literal_open("/jobs/stats", "/jobs/stats"));
}
