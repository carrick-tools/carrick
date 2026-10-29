//! Site questions on the wire (carrick#2437, part of #2441): the scanner sends
//! each discovered site's record and reads back the model's answers.
//!
//! The model runs in the cloud, because its key is a cloud secret
//! (carrick-cloud#2380 is the lambda). This module is the contract and the
//! client. The record is built by #2435 and the answers become rows in #2439;
//! neither is here.
//!
//! - **Switch.** Nothing is sent unless `CARRICK_SITE_QUESTIONS=1`
//!   ([`enabled`]). With it unset the scanner sends nothing new, and every
//!   site is unanswered.
//! - **Route.** `POST /site-questions`, through the agent service's one path
//!   ([`crate::agent_service::AgentService::post_to_lambda`]): the scanner's
//!   credential, the `X-Carrick-Scanner-Version` gate, the pacer, and the
//!   request-level retry on the envelope's `retriable`. A 2xx body is the
//!   answer itself ([`crate::agent_service::SuccessBody::of`]).
//! - **Request.** `{ repo, service, question_set, sites: [{ site_id, state }] }`,
//!   at most [`MAX_SITES_PER_REQUEST`] sites and [`MAX_REQUEST_BYTES`] per
//!   request ([`batches`]). `question_set` is the id this run last received,
//!   or null.
//! - **State.** The site record ([`SiteState`]) with no static question text:
//!   the stated values travel as places, and the cloud reads them out. Which
//!   slots are asked is part of the record, so it is part of the cloud's cache
//!   key.
//! - **Response.** `{ question_set, answers }`, one per site: answered, with
//!   one [`SlotAnswer`] per asked slot, or failed with a code and whether
//!   asking again can help ([`SiteAnswer`]). Each answer is read on its own,
//!   so one unreadable answer costs only its site.
//! - **Failure.** A failed request, a failed site, a budget refusal or a
//!   timeout leaves that site unanswered ([`SiteAnswers::unanswered`]): it
//!   states no row and withdraws no reader row. No scan-health entry and no
//!   customer text. Only a site the cloud called retriable is asked again,
//!   once, in one more round of requests ([`ask`]).
//! - **Cache.** The cloud's, keyed by workspace, question set and the
//!   record's sha. The scanner stores no answer in the blob, so no
//!   `CACHE_VERSION` pairing: a question change moves `question_set` and the
//!   next scan asks again by itself.
//!
//! - **Handler records (R4b, #2386).** A site may instead be
//!   `{ site_id, handler }` ([`HandlerRecord`]), answered `{ site_id, ok, routes }`
//!   ([`RoutesAnswered`]); one variant per request. Pinned in the contract's
//!   top-level `handler` example.
//!
//! Both repos test against one example: `tests/fixtures/site_questions/contract.json`
//! here, vendored byte for byte as carrick-cloud's
//! `lambdas/site-questions/fixtures/contract.json`.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::time::Duration;

use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;

use crate::agent_service::{AgentCallError, AgentService};

/// The route.
pub const ROUTE: &str = "/site-questions";

/// The switch (#2439): on only when set to `1`.
pub const SWITCH: &str = "CARRICK_SITE_QUESTIONS";

/// The offline answer's key: `<CARRICK_MOCK_FIXTURE_DIR>/site-questions/default.json`.
pub const MOCK_SEED: &str = "default";

/// The offline answer's key for handler records (R4b):
/// `<CARRICK_MOCK_FIXTURE_DIR>/site-questions/handler.json`.
pub const HANDLER_MOCK_SEED: &str = "handler";

/// The most sites one request carries (the cloud refuses more).
pub const MAX_SITES_PER_REQUEST: usize = 50;

/// The most bytes one request's body holds (the cloud refuses more). The
/// Lambda sync payload cap is 6 MB and API Gateway allows 30 s.
pub const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;

/// How long one request may take, its transport retries included, before its
/// sites are left unanswered.
pub const ASK_TIMEOUT: Duration = Duration::from_secs(60);

/// Requests in flight at once.
const IN_FLIGHT: usize = 4;

/// The agent service site questions are sent through: its retries sleep on
/// the run's retry budget ([`crate::retry_budget`]), never a budget of
/// their own.
pub fn agent() -> AgentService {
    AgentService::new().with_retry_policy(crate::agent_service::RetryPolicy::RUN_BUDGETED)
}

/// Whether the switch is on in this process.
pub fn enabled() -> bool {
    enabled_from(std::env::var(SWITCH).ok().as_deref())
}

/// Whether this process sends its site and handler questions: the switch is
/// on and the run may ask the model. A no-model pass (`check`, `refresh`,
/// `init`'s read-in, the after-edit hook) builds its records and asks
/// nothing, so a slot only an answer fills stays unfilled (carrick#2907).
pub fn asks() -> bool {
    asks_with(enabled(), crate::local_mode::no_model())
}

/// [`asks`], given the switch and whether the run asks no model.
fn asks_with(enabled: bool, no_model: bool) -> bool {
    enabled && !no_model
}

/// Whether a value of [`SWITCH`] turns site questions on: `1` only.
pub fn enabled_from(value: Option<&str>) -> bool {
    value == Some("1")
}

/// A slot the record can ask or state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Slot {
    Kind,
    Role,
    Name,
    NameArgument,
    PayloadArgument,
    Method,
    Namespace,
}

/// Where the site is: `at` is a repo-relative `file:line`, `code` the source
/// around it with line numbers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub at: String,
    pub code: String,
}

/// The call at the site, for the argument questions: its source and its
/// arguments' source texts in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SiteCall {
    pub text: String,
    pub at: String,
    pub args: Vec<String>,
}

/// Where a value is stated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaceFrom {
    /// The literal the checker gives a name (`name`), at the site or, with
    /// `via`, in a function the site's calls reach.
    Checker,
    /// A string literal, at the site or (with `via`) in a reached function.
    StringLiteral,
    TemplateLiteral,
    ObjectKey,
    /// The request method a library label says `export.member` sends.
    LabelMethod,
    /// The method that label says it sends when none is given.
    LabelMethodDefault,
}

/// One place a value is stated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub from: PlaceFrom,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
}

/// One value the code states, with every place it is stated (at most four);
/// the first place is the one the option names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatedValue {
    pub text: String,
    pub places: Vec<Place>,
}

/// The stated values: at most 24 each, each text at most 120 characters and
/// unique in its list. A request-method word is offered only as a method.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Stated {
    pub names: Vec<StatedValue>,
    pub methods: Vec<StatedValue>,
}

/// The site record (#2435) as the wire carries it.
///
/// `compiler`, `syntax`, `libraries` and `package_network_roles` are the
/// record's own; the wire carries them as they are. `package_network_roles` is
/// absent when there are none, never null; `call` is null when the site has
/// none, never absent. `asked` is never empty: a record with nothing to ask is
/// not sent. A slot is in `asked` or in `forced`, never both, and no path in
/// any field is absolute. A forced `role` is `neither`, `sends`, `handles`, or
/// `scope` for a scope site (#2474), whose open prefix is asked in `name`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SiteState {
    pub point: Point,
    pub compiler: Value,
    pub syntax: Value,
    pub libraries: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_network_roles: Option<Value>,
    pub call: Option<SiteCall>,
    pub stated: Stated,
    pub asked: Vec<Slot>,
    pub forced: BTreeMap<Slot, String>,
}

/// One site to ask about. `site_id` is the scanner's own handle for the
/// site, unique in a run; the cloud reads nothing into it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SiteQuestion {
    pub site_id: String,
    pub state: SiteState,
}

/// One request's body: sites of one variant ([`Question`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionsRequest<Q> {
    pub repo: String,
    pub service: Option<String>,
    pub question_set: Option<String>,
    pub sites: Vec<Q>,
}

/// A request of site records.
pub type SiteQuestionsRequest = QuestionsRequest<SiteQuestion>;

/// A site of one variant on the wire, and how its answer reads.
pub trait Question: Serialize + Clone {
    type Answer;
    /// The scanner's handle for the site, unique in a run.
    fn site_id(&self) -> &str;
    /// One element of a response's `answers`, or `None` when it is neither
    /// the answered nor the failed shape.
    fn read_answer(element: &Value) -> Option<Result<Self::Answer, Unanswered>>;
}

impl Question for SiteQuestion {
    type Answer = Answered;
    fn site_id(&self) -> &str {
        &self.site_id
    }
    fn read_answer(element: &Value) -> Option<Result<Answered, Unanswered>> {
        parse_answer(element).map(|answer| match answer {
            SiteAnswer::Answered(answered) => Ok(*answered),
            SiteAnswer::Failed(failed) => Err(failed),
        })
    }
}

impl Question for HandlerQuestion {
    type Answer = RoutesAnswered;
    fn site_id(&self) -> &str {
        &self.site_id
    }
    fn read_answer(element: &Value) -> Option<Result<RoutesAnswered, Unanswered>> {
        parse_routes_answer(element)
    }
}

/// The most comparisons one handler record asks in a request (R4b, #2386).
pub const MAX_ASKED_COMPARISONS: usize = 16;
/// The most handlers a shared record lists.
pub const MAX_SHARED_HANDLERS: usize = 8;

/// What a handler record is about (R4b).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectKind {
    Entry,
    Label,
    Shared,
}

/// A labelled `receive` the subject is the handler of.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubjectLabel {
    pub package: String,
    pub export: String,
    pub member: Option<String>,
}

/// A handler a shared record's function is reached from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandlerRef {
    pub at: String,
    pub key: String,
}

/// The function the record is about: its body numbered as `point.code` is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandlerSubject {
    pub at: String,
    pub kind: SubjectKind,
    pub key: String,
    pub params: Vec<String>,
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<SubjectLabel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handlers: Option<Vec<HandlerRef>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handler_count: Option<u32>,
}

/// Code of a comparison written in another function.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeWindow {
    pub at: String,
    pub code: String,
}

/// Where a compared literal stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonPosition {
    Equality,
    Case,
    TableKey,
}

/// The read of the request's path a comparison was proven to make.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComparisonRead {
    pub param: String,
    pub steps: Vec<String>,
}

/// Where the chain from the request to a comparison stopped. `text` is the
/// expression it stopped at; `null` (always written) when the hole has none
/// (a `handed_to` or a missing caller argument).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComparisonHole {
    pub reason: String,
    pub text: Option<String>,
}

/// One path or method comparison. `read` and `hole` are always written; at
/// most one is not null.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandlerComparison {
    pub id: u32,
    pub at: String,
    pub literal: String,
    pub position: ComparisonPosition,
    pub expression: String,
    pub read: Option<ComparisonRead>,
    pub hole: Option<ComparisonHole>,
}

/// A request-method literal the record states.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MethodLiteral {
    pub literal: String,
    pub at: String,
}

/// A `startsWith` prefix the record states.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScopePrefix {
    pub prefix: String,
    pub at: String,
    pub delegates: Vec<String>,
}

/// What the code already decides for a comparison; an absent member is asked.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ForcedRoute {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
}

/// The handler record (R4b, #2386): one request handler and every path and
/// method comparison inside it. No question text, no absolute path. Which
/// comparisons are asked is part of the record, so part of the cache key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandlerRecord {
    pub subject: HandlerSubject,
    pub code: Vec<CodeWindow>,
    pub comparisons: Vec<HandlerComparison>,
    pub methods: Vec<MethodLiteral>,
    pub scopes: Vec<ScopePrefix>,
    pub asked: Vec<u32>,
    pub forced: BTreeMap<String, ForcedRoute>,
}

/// One handler to ask about.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandlerQuestion {
    pub site_id: String,
    pub handler: HandlerRecord,
}

/// A request of handler records. A request holds one variant of site, never
/// a mix of `state` and `handler` sites.
pub type HandlerQuestionsRequest = QuestionsRequest<HandlerQuestion>;

/// What the model chose for one comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteChoice {
    Route,
    None,
    CantTell,
}

/// The answer to one asked comparison. `method` is null for the "no method
/// chosen" option; both are null for `none` and `cant_tell`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteAnswer {
    pub id: u32,
    pub choice: RouteChoice,
    pub method: Option<String>,
    pub path: Option<String>,
    pub confidence: f64,
    pub orders_agree: bool,
}

/// An answered handler record: one [`RouteAnswer`] per asked id, in order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RoutesAnswered {
    pub routes: Vec<RouteAnswer>,
}

/// One element of a handler request's `answers`, or `None` when it is neither
/// shape.
pub fn parse_routes_answer(element: &Value) -> Option<Result<RoutesAnswered, Unanswered>> {
    match element.get("ok")?.as_bool()? {
        true => serde_json::from_value::<RoutesAnswered>(element.clone())
            .ok()
            .map(Ok),
        false => Some(Err(Unanswered {
            code: element.get("code")?.as_str()?.to_string(),
            retriable: element.get("retriable")?.as_bool()?,
        })),
    }
}

/// An answered slot's value: a stated value's text (name, method,
/// namespace), an option's id (kind, role), or an argument's index from 0
/// (name_argument, payload_argument).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SlotValue {
    Index(u32),
    Text(String),
}

/// One answered slot. `value` is `None` when the model chose none of the
/// options; `orders_agree` says whether both option orders picked the same
/// one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlotAnswer {
    pub value: Option<SlotValue>,
    pub confidence: f64,
    pub orders_agree: bool,
}

/// An answered site: one slot per asked slot the model was asked, `None` for
/// a slot not asked.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Answered {
    pub kind: Option<SlotAnswer>,
    pub role: Option<SlotAnswer>,
    pub name: Option<SlotAnswer>,
    pub method: Option<SlotAnswer>,
    pub namespace: Option<SlotAnswer>,
    pub name_argument: Option<SlotAnswer>,
    pub payload_argument: Option<SlotAnswer>,
}

/// A site with no answer this scan, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unanswered {
    pub code: String,
    pub retriable: bool,
}

/// One element of a response's `answers`. The answered side is boxed: it is
/// seven slots against a failure's code and flag.
#[derive(Debug, Clone, PartialEq)]
pub enum SiteAnswer {
    Answered(Box<Answered>),
    Failed(Unanswered),
}

/// What a scan's questions of one variant came to.
#[derive(Debug, Clone, PartialEq)]
pub struct Answers<A> {
    /// The cloud's question-set id, from the last response read.
    pub question_set: Option<String>,
    pub answered: BTreeMap<String, A>,
    pub unanswered: BTreeMap<String, Unanswered>,
    /// Requests sent.
    pub requests: usize,
}

impl<A> Default for Answers<A> {
    fn default() -> Self {
        Self {
            question_set: None,
            answered: BTreeMap::new(),
            unanswered: BTreeMap::new(),
            requests: 0,
        }
    }
}

/// What a scan's site questions came to.
pub type SiteAnswers = Answers<Answered>;

/// What a scan's handler questions came to (R4b).
pub type HandlerAnswers = Answers<RoutesAnswered>;

/// A site the scanner did not send: its own request would be over
/// [`MAX_REQUEST_BYTES`].
pub const TOO_LARGE_CODE: &str = "too_large";
/// A response that held no readable answer for a site it was asked about.
pub const NO_ANSWER_CODE: &str = "no_answer";
/// A request that did not answer within [`ASK_TIMEOUT`].
pub const TIMEOUT_CODE: &str = "timeout";

/// A response, each answer kept for [`parse_answer`] to read on its own.
#[derive(Deserialize)]
struct RawResponse {
    #[serde(default)]
    question_set: Option<String>,
    #[serde(default)]
    answers: Vec<Value>,
}

/// Each site's answer or failure, by site id.
pub type ReadAnswers<A> = BTreeMap<String, Result<A, Unanswered>>;

/// The answers a response body for sites of variant `Q` holds, by site id,
/// and its question-set id. An element that cannot be read is skipped, and
/// its site reads as unanswered. A body that is not the response's shape
/// holds none.
pub fn read_response<Q: Question>(text: &str) -> (Option<String>, ReadAnswers<Q::Answer>) {
    let Ok(raw) = serde_json::from_str::<RawResponse>(text) else {
        return (None, BTreeMap::new());
    };
    let answers = raw
        .answers
        .iter()
        .filter_map(|element| {
            let site_id = element.get("site_id")?.as_str()?.to_string();
            Some((site_id, Q::read_answer(element)?))
        })
        .collect();
    (raw.question_set, answers)
}

/// One element of `answers`, or `None` when it is neither shape.
pub fn parse_answer(element: &Value) -> Option<SiteAnswer> {
    match element.get("ok")?.as_bool()? {
        true => serde_json::from_value::<Answered>(element.clone())
            .ok()
            .map(|answered| SiteAnswer::Answered(Box::new(answered))),
        false => Some(SiteAnswer::Failed(Unanswered {
            code: element.get("code")?.as_str()?.to_string(),
            retriable: element.get("retriable")?.as_bool()?,
        })),
    }
}

/// The bytes `site` adds to a request's body.
fn site_bytes<Q: Question>(site: &Q) -> usize {
    serde_json::to_vec(site)
        .map(|b| b.len())
        .unwrap_or(usize::MAX)
}

/// Split `sites` into requests of at most [`MAX_SITES_PER_REQUEST`] sites and
/// [`MAX_REQUEST_BYTES`] of body, in order. A site whose own request would be
/// over the byte ceiling is returned apart and never sent.
pub fn batches<Q: Question>(
    repo: &str,
    service: Option<&str>,
    question_set: Option<&str>,
    sites: Vec<Q>,
) -> (Vec<QuestionsRequest<Q>>, Vec<Q>) {
    let empty = QuestionsRequest {
        repo: repo.to_string(),
        service: service.map(str::to_string),
        question_set: question_set.map(str::to_string),
        sites: Vec::new(),
    };
    let envelope = serde_json::to_vec(&empty).map(|b| b.len()).unwrap_or(0);
    let mut out: Vec<QuestionsRequest<Q>> = Vec::new();
    let mut too_large = Vec::new();
    let mut current = empty.clone();
    let mut bytes = envelope;
    for site in sites {
        let size = site_bytes(&site);
        if envelope.saturating_add(size) > MAX_REQUEST_BYTES {
            too_large.push(site);
            continue;
        }
        // One comma between sites.
        let added = size + usize::from(!current.sites.is_empty());
        if current.sites.len() == MAX_SITES_PER_REQUEST || bytes + added > MAX_REQUEST_BYTES {
            out.push(std::mem::replace(&mut current, empty.clone()));
            bytes = envelope;
        }
        bytes += size + usize::from(!current.sites.is_empty());
        current.sites.push(site);
    }
    if !current.sites.is_empty() {
        out.push(current);
    }
    (out, too_large)
}

/// One round: every request through `send`, [`IN_FLIGHT`] at a time. The
/// sites each request carried, with what came back for each.
/// One request's sites, with what came back for them.
type RoundResult<Q> = (
    Vec<Q>,
    Result<(Option<String>, ReadAnswers<<Q as Question>::Answer>), Unanswered>,
);

async fn round<Q, S, Fut>(requests: Vec<QuestionsRequest<Q>>, send: &S) -> Vec<RoundResult<Q>>
where
    Q: Question,
    S: Fn(QuestionsRequest<Q>) -> Fut,
    Fut: Future<Output = Result<String, AgentCallError>>,
{
    stream::iter(requests)
        .map(|request| async move {
            let sites = request.sites.clone();
            let result = match tokio::time::timeout(ASK_TIMEOUT, send(request)).await {
                Ok(Ok(text)) => Ok(read_response::<Q>(&text)),
                // The transport has already retried what it could.
                Ok(Err(error)) => Err(Unanswered {
                    code: error.code,
                    retriable: false,
                }),
                Err(_) => Err(Unanswered {
                    code: TIMEOUT_CODE.to_string(),
                    retriable: false,
                }),
            };
            (sites, result)
        })
        .buffer_unordered(IN_FLIGHT)
        .collect()
        .await
}

/// Ask about `sites` through `send`, which puts one request on the wire and
/// returns its 2xx body.
///
/// Every site ends answered or unanswered. A site the cloud answers with a
/// retriable failure is asked once more, in a second round carrying only
/// those sites; still failing, it is unanswered. Two sites with one id: the
/// first is asked, the second is dropped. This reads no switch; the caller
/// checks [`enabled`] ([`ask_service`] does).
pub async fn ask<Q, S, Fut>(
    repo: &str,
    service: Option<&str>,
    sites: Vec<Q>,
    send: S,
) -> Answers<Q::Answer>
where
    Q: Question,
    S: Fn(QuestionsRequest<Q>) -> Fut,
    Fut: Future<Output = Result<String, AgentCallError>>,
{
    ask_within(repo, service, sites, send, || {
        !crate::retry_budget::remaining().is_zero()
    })
    .await
}

/// [`ask`], with the second round asked only while `may_retry` says so
/// (carrick#2439): the round of retriable sites draws on the run's retry
/// budget ([`crate::retry_budget`]), the one a scan's patient calls share,
/// so a run that has spent it asks each site once.
pub async fn ask_within<Q, S, Fut, R>(
    repo: &str,
    service: Option<&str>,
    sites: Vec<Q>,
    send: S,
    may_retry: R,
) -> Answers<Q::Answer>
where
    Q: Question,
    S: Fn(QuestionsRequest<Q>) -> Fut,
    Fut: Future<Output = Result<String, AgentCallError>>,
    R: Fn() -> bool,
{
    let mut out = Answers::default();
    let mut seen = BTreeSet::new();
    let mut pending: Vec<Q> = sites
        .into_iter()
        .filter(|site| seen.insert(site.site_id().to_string()))
        .collect();
    for attempt in 0..2 {
        if pending.is_empty() || (attempt > 0 && !may_retry()) {
            break;
        }
        let (requests, too_large) = batches(repo, service, out.question_set.as_deref(), pending);
        for site in too_large {
            out.unanswered.insert(
                site.site_id().to_string(),
                Unanswered {
                    code: TOO_LARGE_CODE.to_string(),
                    retriable: false,
                },
            );
        }
        out.requests += requests.len();
        let mut again = Vec::new();
        for (sites, result) in round(requests, &send).await {
            let (question_set, mut answers) = match result {
                Ok(read) => read,
                Err(unanswered) => {
                    for site in sites {
                        out.unanswered
                            .insert(site.site_id().to_string(), unanswered.clone());
                    }
                    continue;
                }
            };
            if question_set.is_some() {
                out.question_set = question_set;
            }
            for site in sites {
                let site_id = site.site_id().to_string();
                match answers.remove(&site_id) {
                    Some(Ok(answered)) => {
                        out.unanswered.remove(&site_id);
                        out.answered.insert(site_id, answered);
                    }
                    Some(Err(failed)) if failed.retriable && attempt == 0 => {
                        out.unanswered.insert(site_id, failed);
                        again.push(site);
                    }
                    Some(Err(failed)) => {
                        out.unanswered.insert(site_id, failed);
                    }
                    None => {
                        out.unanswered.insert(
                            site_id,
                            Unanswered {
                                code: NO_ANSWER_CODE.to_string(),
                                retriable: false,
                            },
                        );
                    }
                }
            }
        }
        pending = again;
    }
    debug!(
        answered = out.answered.len(),
        unanswered = out.unanswered.len(),
        requests = out.requests,
        "site questions: answered"
    );
    out
}

/// [`ask`] through the agent service, when the switch is on. Off, nothing is
/// sent and nothing is answered. `agent` should retry under
/// [`crate::agent_service::RetryPolicy::RUN_BUDGETED`] ([`agent`]), so the
/// requests' own retries draw on the run's budget too.
pub async fn ask_service(
    agent: &AgentService,
    repo: &str,
    service: Option<&str>,
    sites: Vec<SiteQuestion>,
) -> SiteAnswers {
    ask_service_seeded(agent, repo, service, sites, MOCK_SEED).await
}

/// [`ask_service`] for handler records (R4b): their own requests, never
/// mixed with site records, answered offline from
/// `<CARRICK_MOCK_FIXTURE_DIR>/site-questions/handler.json`.
pub async fn ask_handlers_service(
    agent: &AgentService,
    repo: &str,
    service: Option<&str>,
    sites: Vec<HandlerQuestion>,
) -> HandlerAnswers {
    ask_service_seeded(agent, repo, service, sites, HANDLER_MOCK_SEED).await
}

async fn ask_service_seeded<Q: Question>(
    agent: &AgentService,
    repo: &str,
    service: Option<&str>,
    sites: Vec<Q>,
    seed: &str,
) -> Answers<Q::Answer> {
    if !asks() || sites.is_empty() {
        return Answers::default();
    }
    ask(repo, service, sites, |request| async move {
        agent.post_to_lambda(ROUTE, &request, seed).await
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A no-model pass sends no site or handler question whatever the switch
    /// says: the questions are model calls (carrick#2907).
    #[test]
    fn a_no_model_pass_asks_no_site_question() {
        assert!(asks_with(true, false));
        assert!(!asks_with(true, true));
        assert!(!asks_with(false, false));
        assert!(!asks_with(false, true));
    }
}
