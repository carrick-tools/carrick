//! The other services of the run, read for the service being analysed
//! (carrick#2618).
//!
//! A service's walk follows a call into a function another service of the run
//! declares (an SDK wrapper in a sibling package) through that service's own
//! files and compiler rows ([`crate::request_summary::discovery::discover_with`]).
//! Services are analysed side by side on threads of their own, so a sibling's
//! facts are read once per run, from its own walk, and shared:
//!
//! - **Reached siblings only.** The services whose files some call of this one
//!   is declared in, and transitively those they reach in turn. A service that
//!   calls into no other is read alone, as before.
//! - **Built on first need,** by the first service that reaches it, with the
//!   same discovery a service's own analysis runs, quietly ([`Read::Sibling`]):
//!   no scan stage, no report line. Held until the run ends.
//! - **Who owns a file** comes from each service's file walk, taken once before
//!   any service is read: a sibling's own discovery may not exist yet when a
//!   service asks.
//! - A sibling with no compiler rows is no sibling: its code cannot be walked.
//! - **Read from its walk's slot** ([`walk_lane::Slots`]), filled as the walk
//!   ends. A service's analysis starts once the walks it may read have
//!   ended ([`SiblingRegistry::walks_read_by`], carrick#3085).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use tracing::{debug, info};

use super::{Read, canonical_repo_path, discover_with_positions, service_ignore_patterns};
use super::{service_scan_root, walk_lane};
use crate::config::Config;
use crate::request_summary::RequestSummaryInputs;

/// A service's name in the run log.
fn label_of(service: &Config) -> &str {
    service.service_name.as_deref().unwrap_or("(root)")
}

/// One service of the run, read for another's walk.
pub(super) struct Sibling {
    /// Where its install lives: its packages are asked from here.
    pub(super) root: PathBuf,
    pub(super) inputs: RequestSummaryInputs,
    /// The services whose files this one's calls are declared in, by declared
    /// position.
    outside: Vec<usize>,
    /// What the library store is asked for this service's own calls, taken
    /// once ([`super::LibraryAsk::asked_by`]).
    asked: OnceLock<Option<crate::library_store::Asked>>,
}

impl Sibling {
    /// What this service asks the library store, once per run.
    pub(super) fn asked(
        &self,
        repo_root: &Path,
        site_questions: bool,
    ) -> Option<crate::library_store::Asked> {
        self.asked
            .get_or_init(|| {
                let sites = crate::request_summary::library_sites(&self.inputs);
                super::LibraryAsk::asked_by(
                    &self.inputs,
                    &sites,
                    &self.root,
                    repo_root,
                    site_questions,
                )
            })
            .clone()
    }
}

/// The siblings one service's walk may enter, in declared order.
#[derive(Default, Clone)]
pub(super) struct Siblings {
    reached: Vec<Arc<Sibling>>,
}

impl Siblings {
    /// Their inputs, as the walk takes them.
    pub(super) fn inputs(&self) -> Vec<&RequestSummaryInputs> {
        self.reached.iter().map(|sibling| &sibling.inputs).collect()
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &Sibling> {
        self.reached.iter().map(|sibling| sibling.as_ref())
    }
}

/// Where a service's analysis finds its siblings.
#[derive(Clone, Copy)]
pub(super) struct Neighbours<'a> {
    pub(super) registry: &'a SiblingRegistry,
    /// The service's declared position.
    pub(super) own: usize,
}

impl Neighbours<'_> {
    /// The siblings the service whose read is `inputs` may walk into.
    pub(super) fn reached(&self, inputs: &RequestSummaryInputs) -> Siblings {
        // Nothing reads a walk into a sibling unless the site questions run.
        if !crate::site_questions::enabled() {
            return Siblings::default();
        }
        self.registry.reached(self.own, inputs)
    }
}

/// Every service of the run, and the slot each one's read lands in.
pub(super) struct SiblingRegistry {
    /// The canonical repo path every discovery of the run reads under.
    repo_path: String,
    /// Every service the run declares, analysed or carried, in declared order.
    services: Vec<Config>,
    /// Each declared service's place among the analysed ones.
    analysed_at: Vec<Option<usize>>,
    /// Which declared service owns each file, the first to list it winning
    /// (as the walk's list of siblings is ordered).
    owners: OnceLock<HashMap<PathBuf, usize>>,
    slots: Vec<OnceLock<Option<Arc<Sibling>>>>,
    /// Every declared service's compiler walk, analysed or carried, once it
    /// has ended (carrick#3085), when the run walks. A carried service has
    /// one only when some walked service's rows reach it.
    walks: Option<Arc<walk_lane::Slots>>,
}

impl SiblingRegistry {
    pub(super) fn new(
        repo_path: &str,
        services: &[Config],
        analysed_at: Vec<Option<usize>>,
        walks: Option<Arc<walk_lane::Slots>>,
    ) -> Self {
        Self {
            repo_path: canonical_repo_path(repo_path),
            services: services.to_vec(),
            analysed_at,
            owners: OnceLock::new(),
            slots: services.iter().map(|_| OnceLock::new()).collect(),
            walks,
        }
    }

    /// The walk of the declared service `service`, once it has ended, when
    /// the run walks.
    pub(super) fn walked(&self, service: usize) -> Option<&walk_lane::Walked> {
        self.walks.as_ref()?.get(service)
    }

    /// The declared services whose walks a read of the declared service
    /// `service`, whose rows are `positions`, may enter: the walks its
    /// analysis waits for, and those of every service that reads it
    /// (carrick#3085). The services its rows' calls are declared in, closed
    /// over theirs by the gate.
    ///
    /// A call through an import the compiler did not resolve is followed on
    /// the call graph instead (carrick#2557), into any service, named by no
    /// row. Rows that may hold one read every analysed service's walk, as
    /// every analysis did before the gate, and through those walks' rows the
    /// carried services they reach.
    pub(super) fn walks_read_by(
        &self,
        service: usize,
        positions: &crate::position_rows::PositionIndex,
    ) -> BTreeSet<usize> {
        let mut reads = self.declared_in(positions);
        if positions.may_call_through_an_unresolved_import() {
            info!(
                "The rows of {} may call through an unresolved import; a read of it waits for \
                 every service's walk",
                label_of(&self.services[service])
            );
            reads.extend(
                self.analysed_at
                    .iter()
                    .enumerate()
                    .filter(|(index, at)| *index != service && at.is_some())
                    .map(|(index, _)| index),
            );
        }
        reads
    }

    /// The declared services whose files the calls the rows `positions` hold
    /// are declared in.
    fn declared_in(&self, positions: &crate::position_rows::PositionIndex) -> BTreeSet<usize> {
        let owners = self.owners();
        positions
            .call_rows()
            .filter_map(|row| row.decl_file())
            .filter(|file| !file.starts_with("lib:") && !file.starts_with("pkg:"))
            .collect::<BTreeSet<&str>>()
            .into_iter()
            .filter_map(|file| owners.get(&positions.path_of(file)).copied())
            .collect()
    }

    /// The declared position of the analysed service `analysed`.
    pub(super) fn declared(&self, analysed: usize) -> Option<usize> {
        self.analysed_at.iter().position(|at| *at == Some(analysed))
    }

    fn owners(&self) -> &HashMap<PathBuf, usize> {
        self.owners.get_or_init(|| {
            let mut owners = HashMap::new();
            for (index, service) in self.services.iter().enumerate() {
                let walk = crate::file_finder::walk_service_quietly(
                    &self.repo_path,
                    service,
                    &service_ignore_patterns(service),
                );
                for file in walk.files {
                    owners.entry(file).or_insert(index);
                }
            }
            owners
        })
    }

    /// The declared services that own the files `declared_in`.
    fn owners_of(&self, declared_in: &[PathBuf]) -> BTreeSet<usize> {
        let owners = self.owners();
        declared_in
            .iter()
            .filter_map(|file| owners.get(file).copied())
            .collect()
    }

    /// The siblings of the declared service `own`, whose read is `inputs`:
    /// every service its calls are declared in, and those theirs are in turn.
    /// A run that does not walk can walk into no service.
    pub(super) fn reached(&self, own: usize, inputs: &RequestSummaryInputs) -> Siblings {
        let Some(walks) = self.walks.as_deref() else {
            return Siblings::default();
        };
        let declared_in = inputs.outside_declarations();
        if declared_in.is_empty() {
            return Siblings::default();
        }
        // Each service to read, and whether the rows that named it were
        // joined: without them every call was read from the call graph.
        let joined = inputs.row_calls.is_some();
        let mut pending: Vec<(usize, bool)> = self
            .owners_of(&declared_in)
            .into_iter()
            .map(|index| (index, joined))
            .collect();
        let mut reached: BTreeMap<usize, Arc<Sibling>> = BTreeMap::new();
        let mut passed: BTreeSet<usize> = BTreeSet::from([own]);
        while let Some((index, joined)) = pending.pop() {
            if !passed.insert(index) {
                continue;
            }
            if let Some(sibling) = self.sibling(own, index, joined, walks) {
                let joined = sibling.inputs.row_calls.is_some();
                pending.extend(sibling.outside.iter().map(|next| (*next, joined)));
                reached.insert(index, sibling);
            }
        }
        Siblings {
            reached: reached.into_values().collect(),
        }
    }

    /// The declared service `index` read for a walk of `own`, once for the
    /// run, from its walk.
    ///
    /// The analysis of `own` waits for every walk it may read
    /// ([`Self::walks_read_by`], carrick#3085), so with the rows that named
    /// `index` `joined`, its walk has ended. Rows whose join missed its
    /// deadline (carrick#2574) read every call from the call graph, which the
    /// gate, decided at the walk's end, cannot foresee: then a walk that has
    /// not ended is read as not walked, this once, and the next read tries
    /// again.
    fn sibling(
        &self,
        own: usize,
        index: usize,
        joined: bool,
        walks: &walk_lane::Slots,
    ) -> Option<Arc<Sibling>> {
        let Some(walked) = walks.get(index) else {
            if self.analysed_at[index].is_some() || walks.is_walking(index) {
                info!(
                    "Service {} read service {} before its walk ended; {} is read as not \
                     walked",
                    label_of(&self.services[own]),
                    label_of(&self.services[index]),
                    label_of(&self.services[index]),
                );
                debug_assert!(
                    !joined,
                    "{} was read before its walk ended: the gate missed it",
                    label_of(&self.services[index])
                );
            }
            return None;
        };
        self.slots[index]
            .get_or_init(|| self.read(index, walked))
            .clone()
    }

    fn read(&self, index: usize, walked: &walk_lane::Walked) -> Option<Arc<Sibling>> {
        let service = &self.services[index];
        let label = service.service_name.as_deref().unwrap_or("(root)");
        let started = Instant::now();
        let discovery = discover_with_positions(
            &self.repo_path,
            service,
            Default::default(),
            Some(walked.index()),
            crate::request_summary::ROWS_JOIN_BUDGET,
            // The files only tests import matter to this service's own trace.
            false,
            &Default::default(),
            Read::Sibling,
        )
        .ok()?;
        let inputs = discovery.request_inputs;
        // Without rows nothing in it can be walked into.
        inputs.positions.as_ref()?;
        let outside = self
            .owners_of(&inputs.outside_declarations())
            .into_iter()
            .filter(|owner| *owner != index)
            .collect();
        debug!(
            "Service {label} read for the walks of the services that call into it in {:.1}s",
            started.elapsed().as_secs_f64()
        );
        Some(Arc::new(Sibling {
            root: service_scan_root(&self.repo_path, service),
            inputs,
            outside,
            asked: OnceLock::new(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared<T: Send + Sync>() {}

    /// Services are analysed on threads of their own, and a sibling one of
    /// them builds is read by the others.
    #[test]
    fn a_sibling_is_shared_across_the_service_threads() {
        shared::<Sibling>();
        shared::<SiblingRegistry>();
        shared::<Siblings>();
    }
}
