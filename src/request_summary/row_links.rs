//! What each import binding of a service reaches, as the compiler's
//! per-position rows say (carrick#2413 slice B).
//!
//! Without rows, the module-scope names ([`super::library_sites::LinkedNames`]),
//! instances ([`super::LinkedClients`]) and getters another module declares
//! are reached by name: the call graph's resolution names the first module,
//! and each table follows the modules that re-export it, up to
//! [`super::MAX_REEXPORT_HOPS`]. One specifier anywhere in the service that
//! names nothing the scan can find turns every imported reading off
//! (carrick#2402).
//!
//! With rows, each use of an imported local has an `ident` row naming the
//! declaration it reaches, through every re-export, alias and workspace
//! package the compiler follows. Every use must name the same declaration.
//! It joins the declaring file's tables at the line that file records for
//! the name ([`super::FileIr::declared_at`]). What a namespace publishes,
//! and what a module loaded by a literal specifier publishes, are still the
//! module graph's: no row lists a module's exports.
//!
//! An import whose declaration no row gives, and whose specifier may name a
//! module of the repo, may still be any module of the service. A specifier
//! that names only a package (a declared dependency or a runtime builtin,
//! claimed by no alias) reaches none, so it contests nothing
//! ([`ImportedBindings::names_only_a_package`]). It changes nothing when it is only called or read; when it may
//! change what it holds ([`super::BindingUse::may_change`]), or a file loads
//! a module by a computed specifier, every exported object, instance and
//! published getter is contested ([`RowLinks::open`]).

use super::*;
use crate::position_rows::{Kind, PositionIndex};

/// What one import binding reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RowLink {
    /// A module-scope binding a file of the service declares: the file (as
    /// walked) and the name its tables key the binding by.
    Declared(PathBuf, String),
    /// A declaration in a file of the service that is no module-scope
    /// binding its tables record: a CommonJS export's property, a member of
    /// a namespace declaration, a destructured `const`.
    Unrecorded,
    /// A whole module, with what it publishes as the module graph says.
    Module(Vec<PublishedBinding>),
    /// A declaration outside the repository: the default library or a
    /// package, with the package the compiler names for it
    /// (carrick#2413 slice E).
    Outside(Option<String>),
    /// A declaration in a file of the repository that is not one of the
    /// service's: another workspace package, or code an alias names.
    OtherService,
    /// The rows do not say.
    Hole(HoleReason),
}

impl RowLink {
    /// Whether the import reaches a declaration of the repository, whatever
    /// its specifier says (carrick#2413 slice E): then it is no package's
    /// client.
    pub(crate) fn in_repo(&self) -> bool {
        matches!(
            self,
            RowLink::Declared(..)
                | RowLink::Unrecorded
                | RowLink::Module(_)
                | RowLink::OtherService
        )
    }
}

/// Why every exported object, instance and published getter of a service is
/// contested ([`RowLinks::open`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Open {
    /// An import whose declaration no row gives, and whose specifier may
    /// name a module of the repo, may change what it holds.
    UnreadImport,
    /// A file loads a module by a specifier it computes.
    ComputedLoad,
}

/// Every import binding of a service, with what it reaches.
#[derive(Debug, Default)]
pub(crate) struct RowLinks {
    links: HashMap<(PathBuf, String), RowLink>,
    open: Option<Open>,
    /// Bindings whose row names a recorded name at another line than the
    /// declaring file records it on.
    line_disagreements: usize,
    /// Holes that may change what they hold, skipped because their
    /// specifier names only a package.
    package_holes: usize,
    /// The first imports that opened [`Self::open`]: file, local name,
    /// specifier. Debug log only ([`Self::describe`]).
    open_holes: Vec<(PathBuf, String, String)>,
}

/// How many opening imports [`RowLinks::describe`] names.
const OPEN_HOLES_SHOWN: usize = 5;

impl RowLinks {
    /// What each import of `files` reaches, as `positions` says. `None`
    /// once `deadline` passes (carrick#2564).
    pub(crate) fn of(
        files: &HashMap<PathBuf, FileIr>,
        bindings: &ImportedBindings,
        positions: &PositionIndex,
        deadline: std::time::Instant,
    ) -> Option<Self> {
        let mut links = RowLinks::default();
        let mut paths: Vec<&PathBuf> = files.keys().collect();
        paths.sort();
        for file in paths {
            let ir = &files[file];
            if ir.computed_load {
                links.open.get_or_insert(Open::ComputedLoad);
            }
            let uses: HashMap<&str, Vec<(u32, u32)>> = ir.import_uses().collect();
            let mut locals: Vec<&String> = ir.imported.keys().collect();
            locals.sort();
            for local in locals {
                let link = match uses.get(local.as_str()) {
                    Some(spans) => {
                        links.link_of(files, bindings, positions, file, local, spans, deadline)
                    }
                    // Used only where no row is written (a JSX tag, a
                    // re-export by name).
                    None => RowLink::Hole(HoleReason::RowsMissing),
                };
                // A link the deadline cut short is no link.
                if std::time::Instant::now() >= deadline {
                    return None;
                }
                if matches!(link, RowLink::Hole(_)) && ir.imported[local].may_change() {
                    if bindings.names_only_a_package(file, local) {
                        links.package_holes += 1;
                    } else {
                        links.open = Some(Open::UnreadImport);
                        if links.open_holes.len() < OPEN_HOLES_SHOWN {
                            links.open_holes.push((
                                file.clone(),
                                local.clone(),
                                bindings.source_of(file, local).unwrap_or("?").to_string(),
                            ));
                        }
                    }
                }
                links.links.insert((file.clone(), local.clone()), link);
            }
        }
        Some(links)
    }

    /// What `local`, imported in `file` and used at `spans`, reaches. Past
    /// `deadline` a hole, which [`Self::of`] discards: one binding may be
    /// used at every line of a large file.
    #[allow(clippy::too_many_arguments)]
    fn link_of(
        &mut self,
        files: &HashMap<PathBuf, FileIr>,
        bindings: &ImportedBindings,
        positions: &PositionIndex,
        file: &Path,
        local: &str,
        spans: &[(u32, u32)],
        deadline: std::time::Instant,
    ) -> RowLink {
        type Named<'r> = (
            Option<&'r str>,
            Option<u32>,
            Option<&'r str>,
            Option<&'r str>,
        );
        let mut agreed: Option<Named<'_>> = None;
        for (lo, hi) in spans {
            if std::time::Instant::now() >= deadline {
                return RowLink::Hole(HoleReason::RowsMissing);
            }
            let row = match positions.declaration(file, *lo, *hi, Kind::Ident, true) {
                Ok(row) => row,
                Err(reason) => return RowLink::Hole(reason),
            };
            let named = (
                row.decl_file(),
                row.decl_line(),
                row.symbol(),
                row.decl_package(),
            );
            match agreed {
                None => agreed = Some(named),
                Some(earlier) if earlier == named => {}
                Some(_) => return RowLink::Hole(HoleReason::RowsMissing),
            }
        }
        let package = agreed.and_then(|(_, _, _, package)| package.map(str::to_string));
        let Some((Some(decl_file), decl_line, Some(symbol), _)) = agreed else {
            return RowLink::Outside(package);
        };
        if decl_file.starts_with("lib:")
            || decl_file.starts_with("pkg:")
            || decl_file.split('/').any(|part| part == "node_modules")
        {
            return RowLink::Outside(package);
        }
        let declaring = positions.path_of(decl_file);
        let Some(declared) = files.get(&declaring) else {
            return RowLink::OtherService;
        };
        if declaring == file {
            // Every use names a declaration of the importing file itself:
            // a name declared again over the import, not the import.
            return RowLink::Unrecorded;
        }
        // A module's own symbol is named by its file (carrick#2419).
        if symbol == decl_file {
            return match bindings.get(file, local) {
                Some(ImportedBinding::Module(published)) => RowLink::Module(published.clone()),
                _ => RowLink::Hole(HoleReason::RowsMissing),
            };
        }
        match declared.declared_at.get(symbol) {
            Some((line, key)) if Some(*line) == decl_line => {
                RowLink::Declared(declaring, key.clone())
            }
            Some(_) => {
                self.line_disagreements += 1;
                RowLink::Hole(HoleReason::RowsMissing)
            }
            None => RowLink::Unrecorded,
        }
    }

    /// What `local`, imported in `file`, reaches. `None` for a name the file
    /// does not import.
    pub(crate) fn get(&self, file: &Path, local: &str) -> Option<&RowLink> {
        self.links.get(&(file.to_path_buf(), local.to_string()))
    }

    /// Whether every exported object, instance and published getter of the
    /// service is contested, since a use no row follows may change it.
    pub(crate) fn open(&self) -> bool {
        self.open.is_some()
    }

    /// One line for the run log: the links by kind, the holes by reason,
    /// and why the service's exported values are contested, if they are.
    pub(crate) fn describe(&self, root: &Path) -> String {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for link in self.links.values() {
            let key = match link {
                RowLink::Declared(..) => "declared".to_string(),
                RowLink::Unrecorded => "unrecorded".to_string(),
                RowLink::Module(_) => "module".to_string(),
                RowLink::Outside(_) => "outside".to_string(),
                RowLink::OtherService => "other_service".to_string(),
                RowLink::Hole(reason) => format!(
                    "hole:{}",
                    serde_json::to_value(reason)
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_string))
                        .unwrap_or_default()
                ),
            };
            *counts.entry(key).or_default() += 1;
        }
        let open_holes = if self.open_holes.is_empty() {
            String::new()
        } else {
            let named: Vec<String> = self
                .open_holes
                .iter()
                .map(|(file, local, source)| {
                    format!(
                        "{}: {local} from \"{source}\"",
                        file.strip_prefix(root).unwrap_or(file).display()
                    )
                })
                .collect();
            format!("; open holes: {}", named.join("; "))
        };
        format!(
            "{} import binding(s) {counts:?}, {} on another line; exported values contested: {}{open_holes}",
            self.links.len(),
            self.line_disagreements,
            match self.open {
                None if self.package_holes > 0 => {
                    format!("no ({} hole(s) name only a package)", self.package_holes)
                }
                None => "no".to_string(),
                Some(Open::UnreadImport) => "an import no row follows may change one".to_string(),
                Some(Open::ComputedLoad) =>
                    "a module is loaded by a computed specifier".to_string(),
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixture_walk::Walked;
    use super::super::library_sites::{LibrarySite, MemberUse, MemberWire, library_sites};
    use super::*;

    /// Every use reads as a claimed maker or op.
    fn on_wire(_: &str, _: &MemberUse) -> MemberWire {
        MemberWire::OnWire
    }

    /// One service of `tests/fixtures/position-rows-names/`, with `edit`
    /// applied to its tree (paths under `services/names`; empty text removes the
    /// file), read by discovery with the rows the real sidecar walks
    /// (`with_rows`) or with none, as a run without `CARRICK_POSITION_ROWS`.
    fn walked(edit: &[(&str, &str)], with_rows: bool) -> Option<Walked> {
        super::super::fixture_walk::walked(
            "position-rows-names",
            "services/names",
            "names",
            edit,
            with_rows,
        )
    }

    trait Reads {
        fn sites(&self) -> Vec<LibrarySite>;
        fn link(&self, file: &str, local: &str) -> Option<RowLink>;
    }

    impl Reads for Walked {
        fn sites(&self) -> Vec<LibrarySite> {
            library_sites(&self.inputs).sites
        }

        fn link(&self, file: &str, local: &str) -> Option<RowLink> {
            let (path, _) = self
                .inputs
                .files
                .iter()
                .find(|(path, _)| path.ends_with(file))
                .expect("the file is read");
            self.inputs.links.as_ref()?.get(path, local).cloned()
        }
    }

    /// What `bus.publish(…)` at `use.ts:line` is handed as its topic.
    fn topic(sites: &[LibrarySite], line: u32) -> Option<String> {
        let site = sites
            .iter()
            .find(|site| {
                site.file.ends_with("src/use.ts")
                    && site.line == line
                    && site.member.as_deref() == Some("publish")
            })
            .unwrap_or_else(|| panic!("no publish site at use.ts:{line}: {sites:#?}"));
        site.literal(0, None).map(str::to_string)
    }

    /// The `add` site at `use.ts:line`, if the instance is read through.
    fn add_at(sites: &[LibrarySite], line: u32) -> Option<&LibrarySite> {
        sites.iter().find(|site| {
            site.file.ends_with("src/use.ts")
                && site.line == line
                && site.member.as_deref() == Some("add")
        })
    }

    /// Everything `use.ts` reads through its imports, by name.
    fn reads(walked: &Walked) -> Vec<(&'static str, Option<String>)> {
        let sites = walked.sites();
        vec![
            ("TOPICS.orders", topic(&sites, 8)),
            ("PREFIX", topic(&sites, 9)),
            ("BROKEN", topic(&sites, 10)),
            ("ALIASED.send", topic(&sites, 11)),
            ("LEGACY.send", topic(&sites, 12)),
            (
                "emails",
                add_at(&sites, 13).and_then(|site| site.literal(0, None).map(str::to_string)),
            ),
            (
                "getQueue",
                add_at(&sites, 15)
                    .filter(|site| site.contest(on_wire).is_none())
                    .and_then(|site| site.literal(0, None).map(str::to_string)),
            ),
        ]
    }

    fn read(name: &'static str, text: &str) -> (&'static str, Option<String>) {
        (name, Some(text.to_string()))
    }

    /// carrick#2402: an import the scan cannot follow, only called, turns
    /// every imported reading off without rows. With rows each import reads
    /// as the declaration its rows name, through `export *`, a `paths`
    /// alias and a constant whose type did not resolve, and an import only
    /// called contests nothing. A CommonJS export's property is no
    /// module-scope binding the declaring file records, so it stays unread.
    #[test]
    fn an_unrelated_unfollowable_import_turns_off_nothing_the_rows_name() {
        let Some(off) = walked(&[], false) else {
            return;
        };
        let unread: Vec<_> = reads(&off)
            .into_iter()
            .filter(|(_, text)| text.is_some())
            .collect();
        assert!(unread.is_empty(), "read without rows: {unread:?}");

        let on = walked(&[], true).unwrap();
        assert_eq!(
            reads(&on),
            vec![
                read("TOPICS.orders", "orders.created"),
                read("PREFIX", "svc"),
                read("BROKEN", "broken.topic"),
                read("ALIASED.send", "aliased.send"),
                ("LEGACY.send", None),
                read("emails", "welcome"),
                read("getQueue", "later"),
            ]
        );
        assert!(!on.inputs.links.as_ref().unwrap().open());
        assert_eq!(on.link("src/use.ts", "LEGACY"), Some(RowLink::Unrecorded));
        assert!(matches!(
            on.link("src/other.ts", "setup"),
            Some(RowLink::Hole(HoleReason::UnresolvedImport))
        ));
    }

    /// Without the unfollowable import, both read the same: the rows change
    /// nothing a service the scan can follow reads.
    #[test]
    fn a_service_the_scan_can_follow_reads_the_same_with_rows() {
        let Some(off) = walked(&[("src/other.ts", "")], false) else {
            return;
        };
        let on = walked(&[("src/other.ts", "")], true).unwrap();
        assert_eq!(reads(&on), reads(&off));
        assert_eq!(reads(&on)[0], read("TOPICS.orders", "orders.created"));
    }

    /// An import no row follows that may change what it holds (here, handed
    /// on), or a load by a computed specifier, may reach any exported
    /// object or instance: those read nothing. Text cannot change, so it
    /// still reads.
    #[test]
    fn a_use_no_row_follows_that_may_change_an_object_contests_every_one() {
        for edit in [
            (
                "src/other.ts",
                "import { thing } from \"undeclared-pkg\";\nexport const value = thing;\n",
            ),
            (
                "src/dynamic.ts",
                "export function load(name: string) {\n  return import(name);\n}\n",
            ),
        ] {
            let Some(on) = walked(&[edit], true) else {
                return;
            };
            assert!(on.inputs.links.as_ref().unwrap().open(), "{edit:?}");
            assert_eq!(
                reads(&on),
                vec![
                    ("TOPICS.orders", None),
                    read("PREFIX", "svc"),
                    read("BROKEN", "broken.topic"),
                    ("ALIASED.send", None),
                    ("LEGACY.send", None),
                    ("emails", None),
                    ("getQueue", None),
                ],
                "{edit:?}"
            );
        }
    }

    /// What `use.ts` reads when nothing is contested.
    fn all_read() -> Vec<(&'static str, Option<String>)> {
        vec![
            read("TOPICS.orders", "orders.created"),
            read("PREFIX", "svc"),
            read("BROKEN", "broken.topic"),
            read("ALIASED.send", "aliased.send"),
            ("LEGACY.send", None),
            read("emails", "welcome"),
            read("getQueue", "later"),
        ]
    }

    /// What `use.ts` reads when every exported object and instance is
    /// contested.
    fn all_contested() -> Vec<(&'static str, Option<String>)> {
        vec![
            ("TOPICS.orders", None),
            read("PREFIX", "svc"),
            read("BROKEN", "broken.topic"),
            ("ALIASED.send", None),
            ("LEGACY.send", None),
            ("emails", None),
            ("getQueue", None),
        ]
    }

    /// An import of an unreadable name from a specifier that names only a
    /// package (a declared dependency, a runtime builtin) reaches no module
    /// of the repo, so it contests no exported object, however it is used.
    #[test]
    fn an_unfollowable_import_that_names_only_a_package_contests_nothing() {
        for edit in [
            (
                "src/other.ts",
                "import { Kind } from \"@fixture/bus\";\nexport const value = Kind;\n",
            ),
            (
                "src/other.ts",
                "import { readFileSync } from \"fs\";\nexport const value = readFileSync;\n",
            ),
        ] {
            let Some(on) = walked(&[edit], true) else {
                return;
            };
            let local = if edit.1.contains("Kind") {
                "Kind"
            } else {
                "readFileSync"
            };
            assert_eq!(
                on.link("src/other.ts", local),
                Some(RowLink::Hole(HoleReason::UnresolvedImport)),
                "{edit:?}"
            );
            let links = on.inputs.links.as_ref().unwrap();
            assert!(!links.open(), "{edit:?}");
            assert!(
                links
                    .describe(&on.repo)
                    .contains("1 hole(s) name only a package")
            );
            assert_eq!(reads(&on), all_read(), "{edit:?}");
        }
    }

    /// A specifier that may name a module of the repo still contests every
    /// object: an alias with no file behind it, a declared name an alias
    /// claims with a missing target, a `file:` dependency, and a relative
    /// barrel nothing resolves.
    #[test]
    fn an_unfollowable_import_that_may_name_a_module_of_the_repo_still_contests_every_one() {
        let alias_claim = "{\"compilerOptions\":{\"target\":\"es2022\",\"module\":\"esnext\",\"moduleResolution\":\"bundler\",\"strict\":true,\"skipLibCheck\":true,\"allowJs\":true,\"baseUrl\":\".\",\"paths\":{\"@lib/*\":[\"src/lib/*\"],\"@fixture/bus\":[\"src/gen/bus\"]},\"lib\":[\"es2022\",\"dom\"],\"types\":[]},\"include\":[\"src/**/*\"]}";
        let local_manifest = "{\"name\":\"names-api\",\"private\":true,\"dependencies\":{\"@fixture/bus\":\"^1.0.0\",\"@fixture/queue\":\"^2.0.0\",\"local-pkg\":\"file:../local\"}}";
        let cases: [(&str, Vec<(&str, &str)>); 4] = [
            (
                "thing",
                vec![(
                    "src/other.ts",
                    "import { thing } from \"@lib/missing\";\nexport const value = thing;\n",
                )],
            ),
            (
                "Kind",
                vec![
                    (
                        "src/other.ts",
                        "import { Kind } from \"@fixture/bus\";\nexport const value = Kind;\n",
                    ),
                    ("tsconfig.json", alias_claim),
                ],
            ),
            (
                "thing",
                vec![
                    (
                        "src/other.ts",
                        "import { thing } from \"local-pkg\";\nexport const value = thing;\n",
                    ),
                    ("package.json", local_manifest),
                ],
            ),
            (
                "thing",
                vec![(
                    "src/other.ts",
                    "import { thing } from \"./generated/barrel\";\nexport const value = thing;\n",
                )],
            ),
        ];
        for (local, edit) in cases {
            let Some(on) = walked(&edit, true) else {
                return;
            };
            assert_eq!(
                on.link("src/other.ts", local),
                Some(RowLink::Hole(HoleReason::UnresolvedImport)),
                "{edit:?}"
            );
            let links = on.inputs.links.as_ref().unwrap();
            assert!(links.open(), "{edit:?}");
            // The debug line names the import that opened it.
            let described = links.describe(&on.repo);
            assert!(
                described.contains(&format!(
                    "open holes: services/names/src/other.ts: {local} from \""
                )),
                "{described}"
            );
            assert_eq!(reads(&on), all_contested(), "{edit:?}");
        }
    }

    /// A package import that hands an exported object to an unread callee
    /// contests that object (a use of the object itself), and nothing else.
    #[test]
    fn a_package_import_handed_an_object_contests_only_that_object() {
        let edit = (
            "src/other.ts",
            "import { stamp } from \"@fixture/bus\";\nimport { ALIASED } from \"@lib/aliased\";\nstamp(ALIASED);\n",
        );
        let Some(on) = walked(&[edit], true) else {
            return;
        };
        assert_eq!(
            on.link("src/other.ts", "stamp"),
            Some(RowLink::Hole(HoleReason::UnresolvedImport))
        );
        assert!(!on.inputs.links.as_ref().unwrap().open());
        let mut want = all_read();
        want[3] = ("ALIASED.send", None);
        assert_eq!(reads(&on), want);
    }

    /// A module that writes an object through its import, or a namespace
    /// import of its module used as a value, may change it: that object is
    /// unread, with rows as without, and nothing else is.
    #[test]
    fn a_module_that_may_change_one_object_contests_only_that_one() {
        for (edit, contested) in [
            (
                (
                    "src/writer.ts",
                    "import { ALIASED } from \"@lib/aliased\";\nexport function mutate() {\n  ALIASED.send = \"changed\";\n}\n",
                ),
                "ALIASED.send",
            ),
            (
                (
                    "src/all.ts",
                    "import * as topics from \"./topics\";\nexport const every = topics;\n",
                ),
                "TOPICS.orders",
            ),
        ] {
            let Some(on) = walked(&[edit], true) else {
                return;
            };
            assert!(!on.inputs.links.as_ref().unwrap().open(), "{edit:?}");
            for (name, text) in reads(&on) {
                if name == contested || name == "LEGACY.send" {
                    assert_eq!(text, None, "{name} with {edit:?}");
                } else {
                    assert!(text.is_some(), "{name} with {edit:?}");
                }
            }
        }
    }
}
