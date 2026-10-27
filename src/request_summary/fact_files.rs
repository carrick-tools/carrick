//! The files the fact layer sends to the file reader (carrick#2613). The
//! engine builds this from the library store's first answer, before the
//! reader dispatches. With `CARRICK_FACT_FILES=1` ([`enabled`]) the
//! orchestrator reads it to choose the reader's files (ticket T3); without,
//! it only logs how it compares with the files the hand-written signals
//! select (ticket T1).
//!
//! A file is a fact file when any of these holds (decisions D1-D2 on
//! carrick#2440's plan):
//! - discovery found a site in it, or the site's labelled call is in it;
//! - a decided-sites lead is in it: a discovered call whose name no call of
//!   this code decides. These are the leads known before any model question
//!   is asked; the possible calls the questions add later are not here, and
//!   each of those has a site that already put its file in the set;
//! - a call in it goes through a package the store's first answer leaves
//!   without exports.
//!
//! A call discovery could not follow (a stopped walk, or one of
//! [`Discovery::unfollowed`]'s reasons) selects no file by itself: the
//! orchestrator keeps a register signal's candidate there instead (D4).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::LibrarySites;
use super::decided_sites::Decided;
use super::discovery::Discovery;
use crate::library_store::FirstAnswer;
use crate::services::type_sidecar::LibraryRole;

/// Whether the fact layer chooses the file reader's files
/// (`CARRICK_FACT_FILES=1`, decision D6). Off: it only logs them.
pub fn enabled() -> bool {
    std::env::var("CARRICK_FACT_FILES").is_ok_and(|value| value == "1")
}

/// The reason a call is not followed to the end when the walk found no site
/// and passed a call it could not follow inside
/// ([`Discovery::missed_inside`]).
pub const MISSED_INSIDE: &str = "missed_inside";

/// What discovery recorded for one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Followed {
    /// The walk reached a labelled export.
    Site,
    /// The walk entered code and stopped, for this reason.
    Stopped(String),
    /// The walk entered no code and missed something, for this reason
    /// ([`Discovery::unfollowed`]).
    Unfollowed(String),
    /// The walk ended with nothing missed and no site.
    Finished,
}

impl Followed {
    /// The walk did not reach the end of what the call runs: a stop or an
    /// unfollowed call.
    pub fn short(&self) -> bool {
        matches!(self, Followed::Stopped(_) | Followed::Unfollowed(_))
    }
}

/// Why a file is a fact file. A file may carry several.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Why {
    pub site: bool,
    pub lead: bool,
    pub unanswered_package: bool,
}

/// The fact layer's files for one service, by repo-relative path with `/`
/// separators.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FactFiles {
    pub files: BTreeMap<String, Why>,
    /// The calls each fact file sends the reader as listed HTTP candidates,
    /// by SWC span (D3): sites and leads whose labelled export is an HTTP
    /// client or a server framework, and calls into an unanswered package,
    /// whose role nothing knows yet. A site of another role selects its file
    /// and lists nothing, as today's GraphQL and messaging files do.
    pub listed: BTreeMap<String, BTreeSet<(u32, u32)>>,
    /// What discovery recorded at each call, by file and SWC span.
    pub calls: BTreeMap<(String, u32, u32), Followed>,
    /// The lines of the calls made through an export the store labels a
    /// server framework, or through an instance one built, by file
    /// (carrick#2745). A route registration (`app.get("/x", handler)`) has a
    /// request's shape; the role the store gives the export is what tells
    /// them apart. The orchestrator keeps these calls out of the request
    /// members it joins to callers.
    pub server_framework_calls: BTreeMap<String, BTreeSet<u32>>,
    /// The span starts of the same calls, by file. A summary row stated at
    /// one of these calls is a registration the store names, not a request,
    /// and is withdrawn (`RequestSummaryIndex::withdraw_registrations`).
    pub server_framework_starts: BTreeMap<String, BTreeSet<u32>>,
    /// The spans of the calls whose labelled export is a GraphQL client and
    /// which carry no listed end, by file (carrick#2763). A hook that hands a document to a client executes
    /// an operation; the HTTP reader is never asked about it, so a row the
    /// reader states there is not a request the call makes. The site selects
    /// its file and lists nothing.
    pub graphql_client_calls: BTreeMap<String, BTreeSet<(u32, u32)>>,
    /// Packages the first answer left without exports.
    pub unanswered_packages: usize,
    /// The ask got no answer at all.
    pub ask_failed: bool,
}

/// Whether a call reaching an export of `role` is listed for the HTTP
/// reader.
fn listed_role(role: LibraryRole) -> bool {
    matches!(role, LibraryRole::HttpClient | LibraryRole::ServerFramework)
}

impl FactFiles {
    /// The fact files of a service: `discovery` read through the first
    /// answer's labels, `decided` its leads, `library` its calls through
    /// packages, `first` what the store left unanswered. Paths relative to
    /// `root`.
    pub fn of(
        discovery: &Discovery,
        decided: &Decided,
        library: Option<&LibrarySites>,
        first: &FirstAnswer,
        root: &Path,
    ) -> Self {
        let mut facts = FactFiles {
            unanswered_packages: first.unanswered.len(),
            ask_failed: first.ask_failed,
            ..FactFiles::default()
        };
        for site in &discovery.sites {
            let followed = match (&site.end, &site.stop) {
                (Some(_), _) => Followed::Site,
                (None, Some(stop)) => Followed::Stopped(stop.clone()),
                (None, None) => continue,
            };
            facts.record(&site.file, (site.span_start, site.span_end), followed);
            if let Some(end) = &site.end {
                if end.role == LibraryRole::GraphqlClient {
                    facts
                        .graphql_client_calls
                        .entry(site.file.clone())
                        .or_default()
                        .insert((site.span_start, site.span_end));
                }
                let listed = listed_role(end.role);
                facts.select(&site.file, (site.span_start, site.span_end), listed);
                facts.select(&end.file, (end.span_start, end.span_end), listed);
                facts.files.entry(site.file.clone()).or_default().site = true;
                facts.files.entry(end.file.clone()).or_default().site = true;
            }
        }
        for site in &discovery.unfollowed {
            let reason = site.stop.clone().unwrap_or_default();
            facts.record(
                &site.file,
                (site.span_start, site.span_end),
                Followed::Unfollowed(reason),
            );
        }
        for (file, start, end) in &discovery.missed_inside {
            facts.record(
                file,
                (*start, *end),
                Followed::Unfollowed(MISSED_INSIDE.to_string()),
            );
        }
        for (file, start, end) in &discovery.finished {
            facts.record(file, (*start, *end), Followed::Finished);
        }
        for lead in &decided.leads {
            let listed = serde_json::from_value::<LibraryRole>(lead.end.role.clone().into())
                .is_ok_and(listed_role);
            facts.select(&lead.file, (lead.span_start, lead.span_end), listed);
            facts.select(
                &lead.end.file,
                (lead.end.span_start, lead.end.span_end),
                listed,
            );
            facts.files.entry(lead.file.clone()).or_default().lead = true;
            facts.files.entry(lead.end.file.clone()).or_default().lead = true;
        }
        if let Some(library) = library {
            for site in &library.sites {
                let relative = site.file.strip_prefix(root).unwrap_or(&site.file);
                let file = relative.to_string_lossy().replace('\\', "/");
                if first.exports.iter().any(|claim| {
                    claim.role == LibraryRole::ServerFramework
                        && claim.specifier == site.specifier
                        && claim.export == site.export
                }) {
                    facts
                        .server_framework_calls
                        .entry(file.clone())
                        .or_default()
                        .insert(site.line);
                    facts
                        .server_framework_starts
                        .entry(file.clone())
                        .or_default()
                        .insert(site.span_start);
                }
                if !first.unanswered.contains(site.package()) {
                    continue;
                }
                facts.select(&file, (site.span_start, site.span_end), true);
                facts.files.entry(file).or_default().unanswered_package = true;
            }
        }
        // A call can carry several ends, one site each at the same span. When
        // any of them is listed for the reader (an HTTP client or a server
        // framework), the call makes a real request as well, and the reader's
        // row there is not the GraphQL operation's.
        let listed = &facts.listed;
        facts.graphql_client_calls.retain(|file, spans| {
            spans.retain(|span| !listed.get(file).is_some_and(|l| l.contains(span)));
            !spans.is_empty()
        });
        facts
    }

    /// These fact files, less every file `owns` refuses (carrick#2618): a
    /// walk that enters a sibling service's code ends in a file this service
    /// does not read, and selects no file of its own and lists no span there.
    /// The sibling's own scan states its own.
    pub fn within(mut self, owns: impl Fn(&str) -> bool) -> Self {
        self.files.retain(|file, _| owns(file));
        self.listed.retain(|file, _| owns(file));
        self.calls.retain(|(file, _, _), _| owns(file));
        self.server_framework_calls.retain(|file, _| owns(file));
        self.server_framework_starts.retain(|file, _| owns(file));
        self
    }

    /// Record what discovery found at one call. A site wins over anything
    /// else recorded there, and anything else over a finished walk.
    fn record(&mut self, file: &str, span: (u32, u32), followed: Followed) {
        let key = (file.to_string(), span.0, span.1);
        match self.calls.get(&key) {
            Some(Followed::Site) => {}
            Some(kept) if *kept != Followed::Finished && followed != Followed::Site => {}
            _ => {
                self.calls.insert(key, followed);
            }
        }
    }

    /// List the call at `span` in `file` for the reader, when `listed`.
    fn select(&mut self, file: &str, span: (u32, u32), listed: bool) {
        if listed {
            self.listed
                .entry(file.to_string())
                .or_default()
                .insert(span);
        }
    }

    /// How many fact files hold a site, a lead and an unanswered-package
    /// call, in that order.
    pub fn counts(&self) -> (usize, usize, usize) {
        let count = |why: fn(&Why) -> bool| self.files.values().filter(|why_| why(why_)).count();
        (
            count(|why| why.site),
            count(|why| why.lead),
            count(|why| why.unanswered_package),
        )
    }

    /// The calls the store labels a server framework, as repo-relative file
    /// and call span start pairs.
    pub fn server_framework_start_pairs(&self) -> Vec<(String, u32)> {
        self.server_framework_starts
            .iter()
            .flat_map(|(file, starts)| starts.iter().map(|start| (file.clone(), *start)))
            .collect()
    }

    /// Whether the call on `line` of `module` is made through a server
    /// framework export or an instance one built (carrick#2745). `module` is
    /// compared component-wise by its tail, so a canonical path still matches
    /// the repo-relative key.
    pub fn server_framework_call(&self, module: &Path, line: u32) -> bool {
        self.server_framework_calls
            .iter()
            .any(|(file, lines)| module.ends_with(Path::new(file)) && lines.contains(&line))
    }

    /// Whether the call at `span` in `module` reaches an export the store
    /// labels a GraphQL client (carrick#2763). `module` is compared
    /// component-wise by its tail, so a canonical path still matches the
    /// repo-relative key.
    pub fn graphql_client_call(&self, module: &Path, span: (u32, u32)) -> bool {
        self.graphql_client_calls
            .iter()
            .any(|(file, spans)| module.ends_with(Path::new(file)) && spans.contains(&span))
    }

    /// What discovery recorded at the call at `span` in `file`, or `None`
    /// when it recorded nothing there.
    pub fn followed(&self, file: &str, span: (u32, u32)) -> Option<&Followed> {
        self.calls.get(&(file.to_string(), span.0, span.1))
    }

    /// The spans `file` lists for the reader, none when it lists nothing.
    pub fn listed_in(&self, file: &str) -> BTreeSet<(u32, u32)> {
        self.listed.get(file).cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request_summary::discovery::{DiscoverySite, SiteEnd};

    fn site(
        file: &str,
        span: (u32, u32),
        end: Option<SiteEnd>,
        stop: Option<&str>,
    ) -> DiscoverySite {
        DiscoverySite {
            file: file.to_string(),
            line: 1,
            span_start: span.0,
            span_end: span.1,
            callee: "send".to_string(),
            end,
            chain: Vec::new(),
            stop: stop.map(str::to_string),
            receiver: None,
            reached: None,
        }
    }

    fn end(file: &str, role: LibraryRole) -> SiteEnd {
        SiteEnd {
            package: "lib".to_string(),
            export: "default".to_string(),
            member: None,
            role,
            op: None,
            file: file.to_string(),
            line: 1,
            span_start: 5,
            span_end: 9,
            new: false,
        }
    }

    /// A call with a GraphQL-client end and an HTTP-client end at the same
    /// span makes a real request too: it is no GraphQL-client call, so the
    /// reader's row there is kept. A call with only the GraphQL end stays.
    #[test]
    fn a_call_with_a_listed_end_is_no_graphql_client_call() {
        let discovery = Discovery {
            sites: vec![
                site(
                    "a.ts",
                    (1, 4),
                    Some(end("b.ts", LibraryRole::GraphqlClient)),
                    None,
                ),
                site(
                    "a.ts",
                    (1, 4),
                    Some(end("c.ts", LibraryRole::HttpClient)),
                    None,
                ),
                site(
                    "a.ts",
                    (6, 9),
                    Some(end("b.ts", LibraryRole::GraphqlClient)),
                    None,
                ),
            ],
            ..Discovery::default()
        };
        let facts = FactFiles::of(
            &discovery,
            &Decided::default(),
            None,
            &FirstAnswer::default(),
            Path::new("/repo"),
        );
        assert!(!facts.graphql_client_call(Path::new("/repo/a.ts"), (1, 4)));
        assert!(facts.graphql_client_call(Path::new("/repo/a.ts"), (6, 9)));
    }

    /// A site puts its own file and its labelled call's file in the set; a
    /// stopped walk records the call as stopped and selects nothing; a call
    /// one walk reached and another stopped is a site.
    #[test]
    fn a_site_selects_both_files_and_a_stop_selects_none() {
        let discovery = Discovery {
            sites: vec![
                site(
                    "a.ts",
                    (1, 4),
                    Some(end("b.ts", LibraryRole::HttpClient)),
                    None,
                ),
                site("c.ts", (1, 4), None, Some("no_body")),
                site("d.ts", (1, 4), None, Some("no_body")),
                site("d.ts", (1, 4), Some(end("e.ts", LibraryRole::Broker)), None),
            ],
            ..Discovery::default()
        };
        let facts = FactFiles::of(
            &discovery,
            &Decided::default(),
            None,
            &FirstAnswer::default(),
            Path::new("/repo"),
        );
        let files: Vec<&str> = facts.files.keys().map(String::as_str).collect();
        assert_eq!(files, ["a.ts", "b.ts", "d.ts", "e.ts"]);
        assert_eq!(
            facts.followed("c.ts", (1, 4)),
            Some(&Followed::Stopped("no_body".to_string()))
        );
        assert_eq!(facts.followed("d.ts", (1, 4)), Some(&Followed::Site));
        assert_eq!(facts.followed("a.ts", (9, 9)), None);
        assert_eq!(facts.counts(), (4, 0, 0));
        // The HTTP client's site and its labelled call are listed; the
        // broker's file is read with nothing listed.
        assert_eq!(facts.listed_in("a.ts"), BTreeSet::from([(1, 4)]));
        assert_eq!(facts.listed_in("b.ts"), BTreeSet::from([(5, 9)]));
        assert!(facts.listed_in("d.ts").is_empty());
        assert!(facts.listed_in("e.ts").is_empty());
    }

    /// carrick#2618: a walk that enters a sibling service's code ends in a
    /// file this service does not read. That file selects nothing and lists
    /// no span, while the service's own call that enters it stays a site.
    #[test]
    fn a_file_outside_the_service_selects_nothing() {
        let discovery = Discovery {
            sites: vec![site(
                "a.ts",
                (1, 4),
                Some(end("packages/b.ts", LibraryRole::HttpClient)),
                None,
            )],
            ..Discovery::default()
        };
        let facts = FactFiles::of(
            &discovery,
            &Decided::default(),
            None,
            &FirstAnswer::default(),
            Path::new("/repo"),
        )
        .within(|file| !file.starts_with("packages/"));
        let files: Vec<&str> = facts.files.keys().map(String::as_str).collect();
        assert_eq!(files, ["a.ts"]);
        assert_eq!(facts.listed_in("a.ts"), BTreeSet::from([(1, 4)]));
        assert!(facts.listed_in("packages/b.ts").is_empty());
        assert_eq!(facts.followed("a.ts", (1, 4)), Some(&Followed::Site));
    }

    /// carrick#2725: a route registered on a framework instance handed in as
    /// a parameter, in a module that imports the framework as a type only,
    /// is no finished walk: the service asked the store nothing about the
    /// framework, so its first answer holds no export of it and does not
    /// name it answered, and the call is `unanswered_package`, which D4
    /// keeps. Counting it answered while its exports are dropped (what the
    /// first answer did with a store answer that holds more than was asked)
    /// made both walks finished. The fixture's README says more.
    #[test]
    fn a_route_on_a_framework_parameter_is_no_finished_walk() {
        use super::super::discovery::{Labels, discover};
        let Some(on) = super::super::fixture_walk::walked(
            "discovery-framework",
            "services/api",
            "api",
            &[],
            true,
        ) else {
            return;
        };
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/discovery-framework");
        let answer = crate::library_store::parse_answer(
            &std::fs::read_to_string(fixture.join("library-claims.json")).unwrap(),
        );
        assert_eq!(answer.exports.len(), 1, "the store labels the framework");
        let source =
            std::fs::read_to_string(fixture.join("repo/services/api/src/routes.ts")).unwrap();
        // Each registration's SWC span: byte offset + 1, the call without its `;`.
        let spans: Vec<(u32, u32)> = source
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("app.get(") || line.starts_with("app.post("))
            .map(|line| {
                let call = line.trim_end_matches(';');
                let start = source.find(call).unwrap() as u32 + 1;
                (start, start + call.len() as u32)
            })
            .collect();
        assert_eq!(spans.len(), 2);
        let read = |first: &FirstAnswer| {
            let labels = Labels::of_answered(&first.exports, &first.answered);
            let discovery = discover(&on.inputs, &labels, &on.repo);
            FactFiles::of(&discovery, &Decided::default(), None, first, &on.repo)
        };
        // The first answer this service gets: nothing asked, nothing answered.
        let first = FirstAnswer::default();
        let facts = read(&first);
        for span in &spans {
            assert_eq!(
                facts.followed("services/api/src/routes.ts", *span),
                Some(&Followed::Unfollowed("unanswered_package".to_string())),
                "{span:?}"
            );
        }
        // The framework named answered with its exports dropped: finished.
        let leaked = FirstAnswer {
            answered: answer.answered_packages.clone(),
            ..FirstAnswer::default()
        };
        let facts = read(&leaked);
        for span in &spans {
            assert_eq!(
                facts.followed("services/api/src/routes.ts", *span),
                Some(&Followed::Finished),
                "{span:?}"
            );
        }
    }

    /// An unfollowed call and a finished walk are recorded and select no
    /// file; a finished walk never hides what another record says.
    #[test]
    fn unfollowed_and_finished_calls_are_recorded_and_select_nothing() {
        let discovery = Discovery {
            unfollowed: vec![site("f.ts", (1, 4), None, Some("value_callee"))],
            finished: vec![("f.ts".to_string(), 1, 4), ("g.ts".to_string(), 2, 6)],
            ..Discovery::default()
        };
        let facts = FactFiles::of(
            &discovery,
            &Decided::default(),
            None,
            &FirstAnswer::default(),
            Path::new("/repo"),
        );
        assert!(facts.files.is_empty());
        assert_eq!(
            facts.followed("f.ts", (1, 4)),
            Some(&Followed::Unfollowed("value_callee".to_string()))
        );
        assert_eq!(facts.followed("g.ts", (2, 6)), Some(&Followed::Finished));
        assert!(facts.followed("f.ts", (1, 4)).unwrap().short());
        assert!(!Followed::Finished.short());
    }

    /// carrick#2745: a route registration and a client call have the same
    /// shape; the store's role for the export is what separates them.
    #[test]
    fn a_call_through_a_server_framework_export_is_recorded_by_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical root");
        std::fs::write(
            root.join("package.json"),
            "{ \"name\": \"service\", \"dependencies\": { \"@fixture/server\": \"^1.0.0\", \"@fixture/http\": \"^1.0.0\" } }\n",
        )
        .expect("write manifest");
        let files = [
            (
                "src/app.ts",
                "import server from \"@fixture/server\";\n\nexport function create() {\n  const srv = server();\n  srv.get(\"/health\", () => \"ok\");\n  return srv;\n}\n",
            ),
            (
                "src/status.ts",
                "import http from \"@fixture/http\";\n\nexport function read() {\n  return http.get(\"/api/v1/status\");\n}\n",
            ),
        ];
        for (name, source) in files {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
            std::fs::write(&path, source).expect("write source");
        }
        let sites = super::super::library_sites::library_sites(
            &crate::engine::discover_request_inputs(&root),
        );
        let claim = |package: &str, role: &str| {
            serde_json::json!({
                "package": package, "version": "1.0.0", "status": "answered",
                "surface_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "exports": [{
                    "specifier": package, "export": "default", "role": role,
                    "side": null, "classifier": "fixture-classifier", "makes": [],
                    "scopes": [], "ops": [], "reserved": [], "calls": [], "patterns": []
                }]
            })
        };
        let text = serde_json::json!({
            "library_claims": [
                claim("@fixture/server", "server_framework"),
                claim("@fixture/http", "http_client"),
            ]
        })
        .to_string();
        let first = FirstAnswer {
            exports: crate::library_store::parse_answer(&text).exports,
            ..FirstAnswer::default()
        };
        let facts = FactFiles::of(
            &Discovery::default(),
            &Decided::default(),
            Some(&sites),
            &first,
            &root,
        );
        assert!(facts.server_framework_call(&root.join("src/app.ts"), 5));
        let registered = files[0].1.find("srv.get").expect("the call") as u32 + 1;
        let sent = files[1].1.find("http.get").expect("the call") as u32 + 1;
        assert!(
            facts
                .server_framework_start_pairs()
                .contains(&("src/app.ts".to_string(), registered))
        );
        assert!(
            facts
                .server_framework_start_pairs()
                .iter()
                .all(|(_, start)| *start != sent)
        );
        assert!(!facts.server_framework_call(&root.join("src/status.ts"), 4));
        assert!(!facts.server_framework_call(&root.join("src/other/app.ts"), 5));
    }
}
