//! NAMED-LIBRARY READ (carrick#3422): NestJS URI versioning.
//!
//! Replaced by store labels when carrick#3043 reads decorator routes from the
//! fact layer; delete this module then. Until then this is a named exception
//! to the 2026-10-30 ruling "no fixed lists of library semantics" (David,
//! 2026-11-06): the framework states a route's version in names the decorator
//! pass cannot read as a path (`enableVersioning`, `@Version`,
//! `VERSION_NEUTRAL`, `defaultVersion`, `prefix`), and nothing else in the
//! scanner reads them.
//!
//! Every name below is gated by the decorator pass's own same-origin rule: it
//! must be imported from the module the service's verb decorators come from.

use std::collections::HashSet;
use std::path::Path;

/// What a version expression states, before the module it came from is known
/// to be the verbs' own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawVersion {
    /// No statement.
    Absent,
    Literal(String),
    /// The imported `VERSION_NEUTRAL` identifier, with the module it came from.
    Neutral {
        origin: String,
    },
    /// A statement the scan cannot read (an array, another identifier).
    Unread,
}

/// A decorator route's own version statement, after its origin is checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteVersion {
    /// Nothing on the handler or the controller: the service default applies.
    Inherit,
    Literal(String),
    Neutral,
    Unread,
}

/// What an app-level `enableVersioning({ prefix })` states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionPrefix {
    /// Absent: the framework's own default, `v`.
    Default,
    /// `false`.
    NoPrefix,
    Literal(String),
    /// A template whose unread pieces all come before its literal tail.
    UnreadLead(String),
    Unread,
}

/// One `<app>.enableVersioning({ type: <V>.URI, ... })` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppVersioning {
    /// The module `V` was imported from.
    pub type_origin: String,
    pub default_version: RawVersion,
    pub prefix: VersionPrefix,
}

/// The versioning a service states, once its statements agree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceVersioning {
    pub default_version: DefaultVersion,
    pub prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefaultVersion {
    None,
    Literal(String),
    Unread,
}

/// What resolving a service's statements found, with the counts it owes.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Resolution {
    pub versioning: Option<ServiceVersioning>,
    /// Two URI statements that disagree.
    pub conflicts: usize,
    /// A prefix the scan could not read at all.
    pub prefix_unread: usize,
    /// A prefix read as its literal tail (carrick#3422, D2).
    pub prefix_unread_lead: usize,
}

/// The URI versioning statements in one file.
pub fn app_versioning(_file_path: &Path, _content: &str) -> Vec<AppVersioning> {
    Vec::new()
}

/// The statements of a service, reduced to the one it states.
pub fn resolve(_statements: &[AppVersioning], _verb_origins: &HashSet<String>) -> Resolution {
    Resolution::default()
}

/// The path a route is served under, and whether its version was stated but
/// unread.
pub fn versioned_path(
    _service: &ServiceVersioning,
    _version: &RouteVersion,
    path: &str,
) -> (String, bool) {
    (path.to_string(), false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swc_scanner::{DecoratorRoute, SwcScanner};
    use std::path::PathBuf;

    const FRAMEWORK: &str = "./framework";

    fn statements(content: &str) -> Vec<AppVersioning> {
        app_versioning(&PathBuf::from("main.ts"), content)
    }

    fn origins() -> HashSet<String> {
        HashSet::from([FRAMEWORK.to_string()])
    }

    fn resolved(content: &str) -> Resolution {
        resolve(&statements(content), &origins())
    }

    fn routes(content: &str) -> Vec<DecoratorRoute> {
        let mut routes = SwcScanner::new().decorator_routes(&PathBuf::from("c.ts"), content);
        routes.sort_by_key(|route| route.span_start);
        routes
    }

    /// The URI statement, its default version and its prefix.
    #[test]
    fn uri_versioning_reads_type_default_and_prefix() {
        let header = "import { VersioningType, VERSION_NEUTRAL } from './framework';\n";

        let uri = resolved(&format!(
            "{header}app.enableVersioning({{ type: VersioningType.URI, defaultVersion: '1' }});"
        ));
        assert_eq!(
            uri.versioning,
            Some(ServiceVersioning {
                default_version: DefaultVersion::Literal("1".into()),
                prefix: "v".into(),
            })
        );

        let custom = resolved(&format!(
            "{header}app.enableVersioning({{ type: VersioningType.URI, prefix: 'rev' }});"
        ));
        assert_eq!(
            custom.versioning,
            Some(ServiceVersioning {
                default_version: DefaultVersion::None,
                prefix: "rev".into(),
            })
        );

        let bare = resolved(&format!(
            "{header}app.enableVersioning({{ type: VersioningType.URI, prefix: false, defaultVersion: VERSION_NEUTRAL }});"
        ));
        assert_eq!(
            bare.versioning,
            Some(ServiceVersioning {
                default_version: DefaultVersion::None,
                prefix: String::new(),
            })
        );

        // Another type states nothing.
        let header_type = resolved(&format!(
            "{header}app.enableVersioning({{ type: VersioningType.HEADER, header: 'x' }});"
        ));
        assert_eq!(header_type.versioning, None);

        // A type from a module the verbs do not come from states nothing.
        let foreign = resolve(
            &statements(
                "import { VersioningType } from './other';\napp.enableVersioning({ type: VersioningType.URI });",
            ),
            &origins(),
        );
        assert_eq!(foreign.versioning, None);

        // Two calls that disagree state nothing, and it is counted.
        let disagree = resolved(&format!(
            "{header}a.enableVersioning({{ type: VersioningType.URI, defaultVersion: '1' }});\n\
             b.enableVersioning({{ type: VersioningType.URI, defaultVersion: '2' }});"
        ));
        assert_eq!(disagree.versioning, None);
        assert_eq!(disagree.conflicts, 1);

        // Two that agree state it once.
        let agree = resolved(&format!(
            "{header}a.enableVersioning({{ type: VersioningType.URI }});\n\
             b.enableVersioning({{ type: VersioningType.URI }});"
        ));
        assert!(agree.versioning.is_some());
        assert_eq!(agree.conflicts, 0);

        // A default that is an array is stated and unread.
        let array = resolved(&format!(
            "{header}app.enableVersioning({{ type: VersioningType.URI, defaultVersion: ['1', '2'] }});"
        ));
        assert_eq!(
            array.versioning.map(|v| v.default_version),
            Some(DefaultVersion::Unread)
        );
    }

    /// The nearest statement wins: the handler, then the controller, then the
    /// service default.
    #[test]
    fn decorator_routes_carry_handler_then_controller_version() {
        let found = routes(
            r#"
import { Controller, Get, Version } from './framework';

@Controller({ path: 'orders', version: '2' })
export class Orders {
  @Get()
  list() {}

  @Version('3')
  @Get('late')
  late() {}
}

@Controller('plain')
export class Plain {
  @Get()
  list() {}
}
"#,
        );
        let versions: Vec<(&str, &RouteVersion)> = found
            .iter()
            .map(|route| (route.handler.as_str(), &route.version))
            .collect();
        assert_eq!(
            versions,
            vec![
                ("list", &RouteVersion::Literal("2".into())),
                ("late", &RouteVersion::Literal("3".into())),
                ("list", &RouteVersion::Inherit),
            ]
        );

        let service = ServiceVersioning {
            default_version: DefaultVersion::Literal("1".into()),
            prefix: "v".into(),
        };
        let paths: Vec<String> = found
            .iter()
            .map(|route| versioned_path(&service, &route.version, &route.path).0)
            .collect();
        assert_eq!(paths, vec!["/v2/orders", "/v3/orders/late", "/v1/plain"]);
    }

    /// `VERSION_NEUTRAL` is the identifier imported from the verbs' module, not
    /// a string: a string of that name is a version, and a same-named import
    /// from elsewhere is unread.
    #[test]
    fn version_neutral_is_the_imported_identifier_not_a_string() {
        let found = routes(
            r#"
import { Controller, Get, Version, VERSION_NEUTRAL } from './framework';
import { VERSION_NEUTRAL as ELSEWHERE } from './other';

@Controller({ path: 'a', version: VERSION_NEUTRAL })
export class A {
  @Get()
  neutral() {}

  @Version('VERSION_NEUTRAL')
  @Get('literal')
  literal() {}

  @Version(ELSEWHERE)
  @Get('elsewhere')
  elsewhere() {}
}
"#,
        );
        let versions: Vec<(&str, &RouteVersion)> = found
            .iter()
            .map(|route| (route.handler.as_str(), &route.version))
            .collect();
        assert_eq!(
            versions,
            vec![
                ("neutral", &RouteVersion::Neutral),
                ("literal", &RouteVersion::Literal("VERSION_NEUTRAL".into())),
                ("elsewhere", &RouteVersion::Unread),
            ]
        );

        let service = ServiceVersioning {
            default_version: DefaultVersion::Literal("1".into()),
            prefix: "v".into(),
        };
        let (path, unread) = versioned_path(&service, &RouteVersion::Neutral, "/a");
        assert_eq!((path.as_str(), unread), ("/a", false));
        let (path, unread) = versioned_path(&service, &RouteVersion::Unread, "/a");
        assert_eq!((path.as_str(), unread), ("/a", true));
    }

    /// carrick#3422, D2: a prefix template whose unread pieces all come before
    /// its literal tail reads as the tail.
    #[test]
    fn unread_lead_piece_keeps_the_literal_prefix_tail() {
        let header = "import { VersioningType } from './framework';\n";
        let lead = resolved(&format!(
            "{header}app.enableVersioning({{ type: VersioningType.URI, prefix: `${{BASE}}v` }});"
        ));
        assert_eq!(lead.versioning.map(|v| v.prefix), Some("v".to_string()));
        assert_eq!(lead.prefix_unread_lead, 1);

        // An unread piece AFTER the tail's literal leaves nothing to read.
        let trailing = resolved(&format!(
            "{header}app.enableVersioning({{ type: VersioningType.URI, prefix: `v${{BASE}}` }});"
        ));
        assert_eq!(trailing.versioning, None);
        assert_eq!(trailing.prefix_unread, 1);
    }
}
