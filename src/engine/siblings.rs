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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use tracing::debug;

use super::{Read, canonical_repo_path, discover_with_positions, service_ignore_patterns};
use super::{service_scan_root, walk_lane};
use crate::config::Config;
use crate::request_summary::RequestSummaryInputs;

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
    /// The analysed services' compiler walks, when the run walks.
    pub(super) walks: Option<&'a [walk_lane::Walked]>,
}

impl Neighbours<'_> {
    /// The siblings the service whose read is `inputs` may walk into.
    pub(super) fn reached(&self, inputs: &RequestSummaryInputs) -> Siblings {
        // Nothing reads a walk into a sibling unless the site questions run.
        if !crate::site_questions::enabled() {
            return Siblings::default();
        }
        self.registry.reached(self.own, inputs, self.walks)
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
    /// The compiler walks of the services this run carries (`--changed-only`)
    /// that an analysed service's calls enter, by declared position
    /// ([`walk_carried`]).
    carried: Vec<Option<walk_lane::Walked>>,
}

impl SiblingRegistry {
    pub(super) fn new(
        repo_path: &str,
        services: &[Config],
        analysed_at: Vec<Option<usize>>,
    ) -> Self {
        Self {
            repo_path: canonical_repo_path(repo_path),
            services: services.to_vec(),
            analysed_at,
            owners: OnceLock::new(),
            slots: services.iter().map(|_| OnceLock::new()).collect(),
            carried: services.iter().map(|_| None).collect(),
        }
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

    /// Walk the services this run carries that the analysed services' calls
    /// enter (`--changed-only`, carrick#2618): a carried service has no walk
    /// yet, and a scan that stopped at it would report the cross-service rows
    /// of a change to the consumer alone as removed. Run in a lane round of
    /// its own after the analysed services' walks and before any analysis, so
    /// no walk process is alive beside an analysis sidecar. Rows only: the
    /// service is neither analysed nor uploaded. A carried service one of
    /// those reaches in turn is walked in the next round.
    pub(super) async fn walk_carried(
        &mut self,
        walks: &[walk_lane::Walked],
        recipe: Option<crate::services::type_sidecar::SidecarRecipe>,
        width: usize,
    ) {
        if self.analysed_at.iter().all(Option::is_some) || !crate::site_questions::enabled() {
            return;
        }
        let mut wanted: BTreeSet<usize> = walks
            .iter()
            .flat_map(|walked| self.declared_in(&walked.index()))
            .collect();
        loop {
            let next: Vec<usize> = wanted
                .iter()
                .copied()
                .filter(|index| {
                    self.analysed_at[*index].is_none() && self.carried[*index].is_none()
                })
                .collect();
            if next.is_empty() {
                return;
            }
            let configs: Vec<Config> = next
                .iter()
                .map(|index| self.services[*index].clone())
                .collect();
            debug!(
                "Walking {} carried service(s) the analysed services call into",
                configs.len()
            );
            let done =
                walk_lane::walk_all(recipe.clone(), &self.repo_path, &configs, width, Vec::new())
                    .await;
            for (index, walked) in next.into_iter().zip(done) {
                wanted.extend(self.declared_in(&walked.index()));
                self.carried[index] = Some(walked);
            }
        }
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
    /// `walks` are the analysed services' compiler walks; without them (a run
    /// that does not walk) no service can be walked into.
    pub(super) fn reached(
        &self,
        own: usize,
        inputs: &RequestSummaryInputs,
        walks: Option<&[walk_lane::Walked]>,
    ) -> Siblings {
        let Some(walks) = walks else {
            return Siblings::default();
        };
        let declared_in = inputs.outside_declarations();
        if declared_in.is_empty() {
            return Siblings::default();
        }
        let mut pending: Vec<usize> = self.owners_of(&declared_in).into_iter().collect();
        let mut reached: BTreeMap<usize, Arc<Sibling>> = BTreeMap::new();
        let mut passed: BTreeSet<usize> = BTreeSet::from([own]);
        while let Some(index) = pending.pop() {
            if !passed.insert(index) {
                continue;
            }
            if let Some(sibling) = self.sibling(index, walks) {
                pending.extend(sibling.outside.iter().copied());
                reached.insert(index, sibling);
            }
        }
        Siblings {
            reached: reached.into_values().collect(),
        }
    }

    /// The declared service `index` read for a walk, once for the run.
    fn sibling(&self, index: usize, walks: &[walk_lane::Walked]) -> Option<Arc<Sibling>> {
        self.slots[index]
            .get_or_init(|| self.read(index, walks))
            .clone()
    }

    fn read(&self, index: usize, walks: &[walk_lane::Walked]) -> Option<Arc<Sibling>> {
        let service = &self.services[index];
        let walked = match self.analysed_at[index] {
            Some(analysed) => walks.get(analysed)?,
            None => self.carried[index].as_ref()?,
        };
        let label = service.service_name.as_deref().unwrap_or("(root)");
        let started = Instant::now();
        let discovery = discover_with_positions(
            &self.repo_path,
            service,
            Default::default(),
            Some(walked.index()),
            crate::request_summary::ROWS_JOIN_BUDGET,
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
