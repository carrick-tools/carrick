//! carrick#2437: the site-question wire and its client.
//!
//! `tests/fixtures/site_questions/contract.json` is the one example both
//! repos test against (carrick-cloud vendors it byte for byte as
//! `lambdas/site-questions/fixtures/contract.json` and runs its `request`
//! through the lambda to get its `response`). Here its request is what the
//! scanner's types write, and its response is what the client reads.
//!
//! The tests that touch the process environment are `#[serial]`.

use carrick::agent_service::{AgentCallError, AgentService};
use carrick::site_questions::{
    Answered, HandlerQuestionsRequest, MAX_ASKED_COMPARISONS, MAX_REQUEST_BYTES,
    MAX_SITES_PER_REQUEST, NO_ANSWER_CODE, ROUTE, RouteChoice, SiteQuestion, SiteQuestionsRequest,
    SlotAnswer, SlotValue, TOO_LARGE_CODE, Unanswered, ask, ask_service, ask_within, batches,
    parse_routes_answer, read_response,
};
use serde_json::{Value, json};
use serial_test::serial;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn contract_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/site_questions/contract.json")
}

fn contract() -> Value {
    serde_json::from_str(&std::fs::read_to_string(contract_path()).unwrap()).unwrap()
}

/// The cloud's response, with a question-set id where the file holds its
/// placeholder.
fn response_text(question_set: &str) -> String {
    serde_json::to_string(&contract()["response"])
        .unwrap()
        .replace("{question_set}", question_set)
}

fn request() -> SiteQuestionsRequest {
    serde_json::from_value(contract()["request"].clone()).unwrap()
}

/// The same sha is pinned in carrick-cloud's
/// `lambdas/site-questions/contract.test.js`. A change to the contract
/// changes both repos' copies and both pins.
const CONTRACT_SHA256: &str = "9034f2c74b115e9e77866b6cf79a274a57a57cf2b656e46b1be71b1be77b47c5";

#[test]
fn the_contract_file_is_the_one_the_cloud_pins() {
    let bytes = std::fs::read(contract_path()).unwrap();
    assert_eq!(format!("{:x}", Sha256::digest(&bytes)), CONTRACT_SHA256);
}

/// Every field the cloud reads survives the scanner's types: an absent
/// `package_network_roles` stays absent (the cloud refuses null), a null
/// `call` stays null, and nothing is added.
#[test]
fn the_request_is_what_the_scanner_types_write() {
    let written = serde_json::to_value(request()).unwrap();
    assert_eq!(written, contract()["request"]);
    let first = &written["sites"][0]["state"];
    assert!(first.get("package_network_roles").is_none());
    assert!(first.get("call").is_some());
    let mut no_call = request();
    no_call.sites[0].state.call = None;
    let written = serde_json::to_value(no_call).unwrap();
    assert_eq!(written["sites"][0]["state"]["call"], Value::Null);
}

fn slot(value: Option<SlotValue>, confidence: f64, orders_agree: bool) -> Option<SlotAnswer> {
    Some(SlotAnswer {
        value,
        confidence,
        orders_agree,
    })
}

#[test]
fn the_response_reads_as_two_answers_a_retriable_failure_and_a_final_one() {
    let (question_set, answers) = read_response::<SiteQuestion>(&response_text("qs-1"));
    assert_eq!(question_set.as_deref(), Some("qs-1"));
    assert_eq!(
        answers["s-0001"],
        Ok(Answered {
            name: slot(Some(SlotValue::Text("/v1/orders".into())), 0.58, true),
            name_argument: slot(Some(SlotValue::Index(0)), 0.97, true),
            payload_argument: slot(None, 0.88, true),
            ..Answered::default()
        })
    );
    assert_eq!(
        answers["s-0002"],
        Ok(Answered {
            kind: slot(Some(SlotValue::Text("topic".into())), 0.93, true),
            role: slot(Some(SlotValue::Text("sends".into())), 0.98, true),
            name: slot(Some(SlotValue::Text("order.paid".into())), 0.47, false),
            name_argument: slot(Some(SlotValue::Index(0)), 0.95, true),
            payload_argument: slot(Some(SlotValue::Index(1)), 0.96, true),
            ..Answered::default()
        })
    );
    assert_eq!(
        answers["s-0003"],
        Err(Unanswered {
            code: "model_error".into(),
            retriable: true
        })
    );
    assert_eq!(
        answers["s-0004"],
        Err(Unanswered {
            code: "answer_malformed".into(),
            retriable: false
        })
    );
}

#[test]
fn an_unreadable_answer_costs_only_its_site() {
    let mut response = contract()["response"].clone();
    response["answers"][1]["kind"] = json!("not a slot");
    let (_, answers) = read_response::<SiteQuestion>(&response.to_string());
    assert!(answers.contains_key("s-0001"));
    assert!(!answers.contains_key("s-0002"));
    assert_eq!(read_response::<SiteQuestion>("not json").1.len(), 0);
}

/// Every request a fake transport was handed.
type Sent = Arc<Mutex<Vec<SiteQuestionsRequest>>>;
/// What one fake request answers.
type Reply = std::future::Ready<Result<String, AgentCallError>>;

/// A fake transport: each request is recorded and answered by `reply`.
fn recorder(
    reply: impl Fn(usize, &SiteQuestionsRequest) -> Result<String, AgentCallError> + Clone,
) -> (Sent, impl Fn(SiteQuestionsRequest) -> Reply) {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&sent);
    let send = move |request: SiteQuestionsRequest| {
        let mut log = log.lock().unwrap();
        let n = log.len();
        let answer = reply(n, &request);
        log.push(request);
        std::future::ready(answer)
    };
    (sent, send)
}

/// What the cloud answers a retried s-0003 the second time.
fn retried_ok() -> String {
    json!({
        "question_set": "qs-1",
        "answers": [{ "site_id": "s-0003", "ok": true, "kind": null, "role": null,
            "name": { "value": "/v1/orders", "confidence": 0.9, "orders_agree": true },
            "method": null, "namespace": null, "name_argument": null, "payload_argument": null }]
    })
    .to_string()
}

#[tokio::test]
async fn only_the_retriable_site_is_asked_again_and_carries_the_question_set() {
    let (sent, send) = recorder(|n, _| {
        Ok(if n == 0 {
            response_text("qs-1")
        } else {
            retried_ok()
        })
    });
    let req = request();
    let out = ask(&req.repo, req.service.as_deref(), req.sites, send).await;
    let sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 2);
    assert_eq!(out.requests, 2);
    assert_eq!(sent[0].question_set, None);
    assert_eq!(sent[1].question_set.as_deref(), Some("qs-1"));
    let again: Vec<&str> = sent[1].sites.iter().map(|s| s.site_id.as_str()).collect();
    assert_eq!(again, ["s-0003"]);
    assert_eq!(
        out.answered.keys().collect::<Vec<_>>(),
        ["s-0001", "s-0002", "s-0003"]
    );
    assert_eq!(out.unanswered.keys().collect::<Vec<_>>(), ["s-0004"]);
    assert_eq!(out.question_set.as_deref(), Some("qs-1"));
}

#[tokio::test]
async fn a_site_still_failing_after_its_second_ask_is_unanswered() {
    let (sent, send) = recorder(|_, _| Ok(response_text("qs-1")));
    let req = request();
    let out = ask(&req.repo, req.service.as_deref(), req.sites, send).await;
    assert_eq!(
        sent.lock().unwrap().len(),
        2,
        "asked again once, never a third time"
    );
    assert_eq!(
        out.unanswered["s-0003"],
        Unanswered {
            code: "model_error".into(),
            retriable: true
        }
    );
    assert_eq!(out.answered.len(), 2);
}

/// carrick#2439: the second round draws on the run's retry budget. With it
/// spent, a retriable site is asked once and stays unanswered.
#[tokio::test]
async fn a_spent_retry_budget_asks_each_site_once() {
    let (sent, send) = recorder(|_, _| Ok(response_text("qs-1")));
    let req = request();
    let out = ask_within(&req.repo, req.service.as_deref(), req.sites, send, || false).await;
    assert_eq!(sent.lock().unwrap().len(), 1, "no second round");
    assert_eq!(
        out.unanswered["s-0003"],
        Unanswered {
            code: "model_error".into(),
            retriable: true
        }
    );
    assert_eq!(out.answered.len(), 2);
}

#[tokio::test]
async fn a_failed_request_leaves_its_sites_unanswered_and_is_not_asked_again() {
    let (sent, send) = recorder(|_, _| {
        Err(AgentCallError {
            code: "rate_limited".into(),
            message: "slow down".into(),
            retriable: true,
            cloud_stated: true,
            details: None,
        })
    });
    let req = request();
    let out = ask(&req.repo, req.service.as_deref(), req.sites, send).await;
    assert_eq!(
        sent.lock().unwrap().len(),
        1,
        "the transport has already retried"
    );
    assert!(out.answered.is_empty());
    assert_eq!(out.unanswered.len(), 4);
    assert!(
        out.unanswered
            .values()
            .all(|u| u.code == "rate_limited" && !u.retriable)
    );
}

#[tokio::test]
async fn a_site_the_response_does_not_answer_is_unanswered() {
    let (_, send) =
        recorder(|_, _| Ok(json!({ "question_set": "qs-1", "answers": [] }).to_string()));
    let req = request();
    let out = ask(&req.repo, req.service.as_deref(), req.sites, send).await;
    assert!(
        out.unanswered
            .values()
            .all(|u| u.code == NO_ANSWER_CODE && !u.retriable)
    );
}

#[tokio::test]
async fn a_site_id_sent_twice_is_asked_once() {
    let (sent, send) = recorder(|_, _| Ok(retried_ok()));
    let mut req = request();
    let mut twin = req.sites[2].clone();
    twin.state.point.at = "elsewhere.ts:1".into();
    req.sites = vec![req.sites[2].clone(), twin];
    ask(&req.repo, None, req.sites, send).await;
    assert_eq!(sent.lock().unwrap()[0].sites.len(), 1);
}

/// `n` copies of the fixture's first site, each with its own id and `pad`
/// bytes of code.
fn sites(n: usize, pad: usize) -> Vec<SiteQuestion> {
    let base = request().sites[0].clone();
    (0..n)
        .map(|i| {
            let mut s = base.clone();
            s.site_id = format!("s-{i:05}");
            s.state.point.code = "x".repeat(pad);
            s
        })
        .collect()
}

#[test]
fn batches_split_at_fifty_sites() {
    let (requests, too_large) = batches(
        "o/r",
        Some("api"),
        None,
        sites(MAX_SITES_PER_REQUEST + 1, 10),
    );
    assert!(too_large.is_empty());
    assert_eq!(
        requests.iter().map(|r| r.sites.len()).collect::<Vec<_>>(),
        [50, 1]
    );
    assert!(
        requests
            .iter()
            .all(|r| r.repo == "o/r" && r.service.as_deref() == Some("api"))
    );
}

#[test]
fn batches_split_at_two_megabytes_and_every_body_fits() {
    // 300 KB a site: six fit under 2 MB, the seventh does not.
    let all = sites(20, 300 * 1024);
    let (requests, too_large) = batches("o/r", None, Some("qs-1"), all);
    assert!(too_large.is_empty());
    assert_eq!(requests.iter().map(|r| r.sites.len()).sum::<usize>(), 20);
    assert!(requests.len() >= 4);
    for r in &requests {
        let body = serde_json::to_vec(r).unwrap();
        assert!(
            body.len() <= MAX_REQUEST_BYTES,
            "a body of {} bytes",
            body.len()
        );
        assert_eq!(r.question_set.as_deref(), Some("qs-1"));
    }
    // A request one site larger than any batch would be over the ceiling.
    let first = &requests[0];
    let mut more = first.clone();
    more.sites.push(requests[1].sites[0].clone());
    assert!(serde_json::to_vec(&more).unwrap().len() > MAX_REQUEST_BYTES);
}

#[tokio::test]
async fn a_site_over_the_ceiling_on_its_own_is_never_sent() {
    let mut all = sites(2, 10);
    all[1].state.point.code = "x".repeat(MAX_REQUEST_BYTES);
    let big = all[1].site_id.clone();
    let (sent, send) =
        recorder(|_, _| Ok(json!({ "question_set": "qs-1", "answers": [] }).to_string()));
    let out = ask("o/r", None, all, send).await;
    assert!(
        sent.lock()
            .unwrap()
            .iter()
            .all(|r| r.sites.iter().all(|s| s.site_id != big))
    );
    assert_eq!(
        out.unanswered[&big],
        Unanswered {
            code: TOO_LARGE_CODE.into(),
            retriable: false
        }
    );
}

fn site_requests() -> usize {
    carrick::agent_service::request_counts()
        .get(ROUTE)
        .copied()
        .unwrap_or(0)
}

/// Offline, with no switch set (carrick#3283), the answers come from the
/// cassette (`<CARRICK_MOCK_FIXTURE_DIR>/site-questions/default.json`) at
/// $0.
#[tokio::test]
#[serial]
async fn offline_the_cassette_answers() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("site-questions")).unwrap();
    std::fs::write(
        dir.path().join("site-questions/default.json"),
        response_text("qs-cassette"),
    )
    .unwrap();
    // SAFETY: every test in this binary that touches the environment is `#[serial]`.
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::set_var(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", dir.path().display()),
        );
        // The retired switch, removed so the environment cannot mask a
        // scanner that still reads it.
        std::env::remove_var("CARRICK_SITE_QUESTIONS");
        std::env::remove_var("CARRICK_NO_MODEL");
    }
    let agent = AgentService::new();
    let req = request();

    let before = site_requests();
    let on = ask_service(&agent, &req.repo, req.service.as_deref(), req.sites.clone()).await;
    assert_eq!(on.question_set.as_deref(), Some("qs-cassette"));
    assert_eq!(on.answered.keys().collect::<Vec<_>>(), ["s-0001", "s-0002"]);
    // The cassette answers s-0003 retriable both times.
    assert_eq!(site_requests(), before + 2);
    assert!(on.unanswered["s-0003"].retriable);
}

/// Offline with no cassette, every site is unanswered and the scan goes on.
#[tokio::test]
#[serial]
async fn offline_with_no_cassette_no_site_is_answered() {
    let dir = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("CARRICK_MOCK_ALL", "1");
        std::env::set_var(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", dir.path().display()),
        );
    }
    let req = request();
    let out = ask_service(&AgentService::new(), &req.repo, None, req.sites).await;
    assert!(out.answered.is_empty());
    assert!(out.unanswered.values().all(|u| u.code == NO_ANSWER_CODE));
}

// ---------------------------------------------------------------------------
// R4b, carrick#2386: the handler record and its `routes` answer. The cloud
// pins the same file by sha and reads `handler.request`.

fn handler_request() -> Value {
    contract()["handler"]["request"].clone()
}

/// The request is what the scanner's types write: every field the cloud reads
/// survives, and nothing is added.
#[test]
fn the_handler_request_is_what_the_scanner_types_write() {
    let typed: HandlerQuestionsRequest = serde_json::from_value(handler_request()).unwrap();
    assert_eq!(serde_json::to_value(&typed).unwrap(), handler_request());
    assert_eq!(typed.sites.len(), 4);
    assert!(typed.sites.iter().all(|s| {
        s.handler.asked.len() <= MAX_ASKED_COMPARISONS
            && s.handler
                .asked
                .iter()
                .all(|id| s.handler.comparisons.iter().any(|c| c.id == *id))
    }));
    // `read` and `hole` are always written, null when absent.
    let first = &serde_json::to_value(&typed).unwrap()["sites"][0]["handler"]["comparisons"][0];
    assert!(first.get("read").is_some() && first["read"].is_null());
    // A hole with no text (a `handed_to` hole) writes `text: null`, never omits it.
    let hole =
        &serde_json::to_value(&typed).unwrap()["sites"][3]["handler"]["comparisons"][1]["hole"];
    assert_eq!(hole["reason"], "handed_to");
    assert!(hole.get("text").is_some() && hole["text"].is_null());
    assert!(
        typed.sites[3].handler.comparisons[1]
            .hole
            .as_ref()
            .unwrap()
            .text
            .is_none()
    );
    // A request holds one variant of site: a handler site has no `state`.
    assert!(handler_request()["sites"][0].get("state").is_none());
}

#[test]
fn the_handler_response_reads_as_a_route_a_none_a_cant_tell_and_a_retriable_failure() {
    let response = &contract()["handler"]["response"];
    let read = |i: usize| parse_routes_answer(&response["answers"][i]).unwrap();
    let route = read(0).unwrap().routes.remove(0);
    assert_eq!(
        (route.choice, route.method.as_deref(), route.path.as_deref()),
        (RouteChoice::Route, Some("GET"), Some("/reports"))
    );
    let none = read(1).unwrap().routes.remove(0);
    assert_eq!(
        (none.choice, none.method, none.path),
        (RouteChoice::None, None, None)
    );
    let cant = read(2).unwrap().routes.remove(0);
    assert_eq!(cant.choice, RouteChoice::CantTell);
    assert!(!cant.orders_agree);
    let failed = read(3).unwrap_err();
    assert_eq!(
        (failed.code.as_str(), failed.retriable),
        ("model_error", true)
    );
    // Each answer holds exactly the ids its record asked, in order.
    let request: HandlerQuestionsRequest = serde_json::from_value(handler_request()).unwrap();
    for (i, site) in request.sites.iter().enumerate().take(3) {
        let ids: Vec<u32> = read(i).unwrap().routes.iter().map(|r| r.id).collect();
        assert_eq!(ids, site.handler.asked);
    }
}

/// R4b: handler records go through the same client as site records, in
/// requests of their own: the contract's response read per record, and
/// only the record the cloud called retriable asked again, once.
#[tokio::test]
async fn handler_records_are_asked_through_the_same_client() {
    let request: HandlerQuestionsRequest = serde_json::from_value(handler_request()).unwrap();
    let retried = request.sites[3].site_id.clone();
    let sent: Arc<Mutex<Vec<HandlerQuestionsRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&sent);
    let first = serde_json::to_string(&contract()["handler"]["response"]).unwrap();
    let again = json!({
        "question_set": "qs-h",
        "answers": [{ "site_id": retried, "ok": true, "routes": [] }]
    })
    .to_string();
    let send = move |request: HandlerQuestionsRequest| {
        let mut log = log.lock().unwrap();
        let reply = if log.is_empty() {
            first.clone()
        } else {
            again.clone()
        };
        log.push(request);
        std::future::ready(Ok::<String, AgentCallError>(reply))
    };
    let out = ask(
        &request.repo,
        request.service.as_deref(),
        request.sites.clone(),
        send,
    )
    .await;
    let sent = sent.lock().unwrap();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1].sites.len(), 1);
    assert_eq!(sent[1].sites[0].site_id, retried);
    assert_eq!(out.answered.len(), request.sites.len());
    assert!(out.unanswered.is_empty());
    let site = &request.sites[0];
    assert_eq!(
        out.answered[&site.site_id]
            .routes
            .iter()
            .map(|r| r.id)
            .collect::<Vec<_>>(),
        site.handler.asked
    );
}

#[test]
fn an_unreadable_routes_answer_is_not_a_shape() {
    let bad = json!({"site_id": "h-1", "ok": true, "routes": [{"id": 0, "choice": "maybe"}]});
    assert!(parse_routes_answer(&bad).is_none());
}
