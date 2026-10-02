//! Which pub/sub and socket rows may pair, given where their names mean
//! something (carrick#1564 section 4, carrick#1663).
//!
//! The exact-key protocols pair a call with a producer when their operation
//! keys are equal: the protocol, the name exactly as written, and for a socket
//! the direction. A row stated through a verified library claim also says
//! where its name means something ([`NameScope`]), and [`names_pair`] decides
//! whether two rows that already share one key may pair because of it.
//!
//! The scope travels beside the key and is never folded into the name or
//! parsed out of it: real names contain `:` and `@`.
//!
//! The cloud pairs socket and pub/sub rows in its own TypeScript (`namesPair`,
//! carrick-cloud#1570), so both readers run one language-neutral vector file,
//! `crates/carrick-match/tests/fixtures/name-scope-pairing.vectors.json`,
//! pinned by sha256 on both sides. Change the vectors first, then both readers.

/// The scope of a name every service shares, such as a broker topic.
pub const GLOBAL_SCOPE: &str = "global";

/// The scope of an id one deployment owns, such as a task id. Every
/// in-process bus row is written with it, so an in-process event never pairs
/// across services.
pub const SERVICE_SCOPE: &str = "service";

/// Where a pub/sub or socket name means something: the row's wire object
/// `name_scope: { scope, namespace }`.
///
/// `scope` is open. Besides [`GLOBAL_SCOPE`] and [`SERVICE_SCOPE`] it can hold
/// a value a newer writer uses, which [`names_pair`] restricts like
/// `service`. `namespace` is the kind of id (`task`, `input_stream`) or
/// `None`. A stated `None` is a value of its own, written `null`, and never a
/// wildcard.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct NameScope {
    pub scope: String,
    #[cfg_attr(feature = "serde", serde(default))]
    pub namespace: Option<String>,
}

impl NameScope {
    /// Whether this name means the same thing in every service. Only
    /// [`GLOBAL_SCOPE`] does: any other value, one this build has no word for
    /// included, keeps the row inside its own service, because a false edge
    /// costs more than a lost one.
    pub fn crosses_services(&self) -> bool {
        self.scope == GLOBAL_SCOPE
    }
}

/// One side of a candidate pairing: the service the row belongs to and the
/// scope its name states, if it states one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopedRow<'a> {
    pub service: &'a str,
    pub name_scope: Option<&'a NameScope>,
}

/// Whether a call row and a producer row that already share one exact
/// operation key may pair.
///
/// - Scope: when either row states a scope other than [`GLOBAL_SCOPE`], the
///   two pair only inside one service.
/// - Namespace: when both rows state a [`NameScope`], their namespaces must be
///   equal, `None` with `None` only.
/// - A row that states nothing adds no restriction of its own, so an index
///   written before the field pairs exactly as it did.
///
/// Every surface that pairs socket or pub/sub rows calls this after the key
/// comparison, never instead of it.
pub fn names_pair(call: ScopedRow<'_>, producer: ScopedRow<'_>) -> bool {
    let crosses = |row: &ScopedRow<'_>| row.name_scope.is_none_or(NameScope::crosses_services);
    if !(crosses(&call) && crosses(&producer)) && call.service != producer.service {
        return false;
    }
    match (call.name_scope, producer.name_scope) {
        (Some(call), Some(producer)) => call.namespace == producer.namespace,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(scope: &str, namespace: Option<&str>) -> NameScope {
        NameScope {
            scope: scope.to_string(),
            namespace: namespace.map(str::to_string),
        }
    }

    fn row<'a>(service: &'a str, name_scope: Option<&'a NameScope>) -> ScopedRow<'a> {
        ScopedRow {
            service,
            name_scope,
        }
    }

    #[test]
    fn rows_that_state_nothing_pair_across_services_as_before() {
        assert!(names_pair(row("a", None), row("b", None)));
        assert!(names_pair(row("a", None), row("a", None)));
    }

    #[test]
    fn the_scope_clause_binds_whichever_row_states_it() {
        let task = scope(SERVICE_SCOPE, Some("task"));
        assert!(!names_pair(row("b", Some(&task)), row("a", None)));
        assert!(!names_pair(row("b", None), row("a", Some(&task))));
        assert!(names_pair(row("a", None), row("a", Some(&task))));
    }

    #[test]
    fn a_scope_this_build_has_no_word_for_restricts_like_service() {
        let region = scope("region", None);
        assert!(!region.crosses_services());
        assert!(!names_pair(
            row("b", Some(&region)),
            row("a", Some(&region))
        ));
        assert!(names_pair(row("a", Some(&region)), row("a", Some(&region))));
    }

    #[test]
    fn a_stated_null_namespace_is_a_value_not_a_wildcard() {
        let null = scope(GLOBAL_SCOPE, None);
        let task = scope(GLOBAL_SCOPE, Some("task"));
        assert!(!names_pair(row("b", Some(&task)), row("a", Some(&null))));
        assert!(names_pair(row("b", Some(&null)), row("a", Some(&null))));
        assert!(names_pair(row("b", None), row("a", Some(&task))));
    }
}
