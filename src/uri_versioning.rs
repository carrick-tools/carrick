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

use std::collections::{HashMap, HashSet};
use std::path::Path;

use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

use crate::swc_scanner::{
    collect_import_locals, decorator_call, literal_string, parse_standalone_module, unwrap_expr,
};

/// The framework's name for "this route has no version segment".
const NEUTRAL_VERSION: &str = "VERSION_NEUTRAL";
/// The framework's default prefix for a URI version segment.
const DEFAULT_PREFIX: &str = "v";

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

impl RawVersion {
    /// The statement as a route's own, once the module the neutral identifier
    /// came from is checked against the module the route's verb came from.
    pub fn for_origin(&self, verb_origin: &str) -> RouteVersion {
        match self {
            Self::Absent => RouteVersion::Inherit,
            Self::Literal(text) => RouteVersion::Literal(text.clone()),
            Self::Neutral { origin } if origin == verb_origin => RouteVersion::Neutral,
            Self::Neutral { .. } | Self::Unread => RouteVersion::Unread,
        }
    }
}

/// What a version expression states: a string, the imported neutral
/// identifier, or something the scan cannot read.
fn read_version(expr: &Expr, imports: &HashMap<String, String>) -> RawVersion {
    let expr = unwrap_expr(expr);
    if let Some(text) = literal_string(expr) {
        return RawVersion::Literal(text);
    }
    if let Expr::Ident(ident) = expr
        && &*ident.sym == NEUTRAL_VERSION
        && let Some(origin) = imports.get(&ident.sym.to_string())
    {
        return RawVersion::Neutral {
            origin: origin.clone(),
        };
    }
    RawVersion::Unread
}

/// The value of the property `name` in an object literal, `Err(())` when a
/// spread, a computed key or a shorthand of that name hides it.
fn object_property<'a>(object: &'a ObjectLit, name: &str) -> Result<Option<&'a Expr>, ()> {
    let mut found = None;
    for prop in &object.props {
        let PropOrSpread::Prop(prop) = prop else {
            return Err(());
        };
        match &**prop {
            Prop::KeyValue(kv) => match &kv.key {
                PropName::Ident(id) if &*id.sym == name => found = Some(&*kv.value),
                PropName::Str(s) if s.value.to_string_lossy() == name => found = Some(&*kv.value),
                PropName::Computed(_) => return Err(()),
                _ => {}
            },
            Prop::Shorthand(id) if &*id.sym == name => return Err(()),
            _ => {}
        }
    }
    Ok(found)
}

/// The `version` a controller decorator's object argument states. The string
/// form states none.
pub(crate) fn controller_version(
    arg: &ExprOrSpread,
    imports: &HashMap<String, String>,
) -> RawVersion {
    let Expr::Object(object) = unwrap_expr(&arg.expr) else {
        return RawVersion::Absent;
    };
    match object_property(object, "version") {
        Ok(None) => RawVersion::Absent,
        Ok(Some(value)) => read_version(value, imports),
        Err(()) => RawVersion::Unread,
    }
}

/// The version a handler states with `@Version(...)`, when that decorator
/// comes from the module its verb decorator came from.
pub(crate) fn handler_version(
    decorators: &[Decorator],
    imports: &HashMap<String, String>,
    verb_origin: &str,
    origin_of: impl Fn(&str) -> String,
) -> RawVersion {
    for decorator in decorators {
        let Some((name, args)) = decorator_call(decorator) else {
            continue;
        };
        if name != "Version" || origin_of(&name) != verb_origin {
            continue;
        }
        return match args {
            [arg] if arg.spread.is_none() => read_version(&arg.expr, imports),
            _ => RawVersion::Unread,
        };
    }
    RawVersion::Absent
}

/// What `prefix` states: `false`, a string, a template whose unread pieces all
/// come before its literal tail (carrick#3422, D2), or nothing readable.
fn read_prefix(expr: &Expr) -> VersionPrefix {
    let expr = unwrap_expr(expr);
    if matches!(expr, Expr::Lit(Lit::Bool(flag)) if !flag.value) {
        return VersionPrefix::NoPrefix;
    }
    if let Some(text) = literal_string(expr) {
        return VersionPrefix::Literal(text);
    }
    if let Expr::Tpl(tpl) = expr {
        let tail = tpl
            .quasis
            .last()
            .and_then(|quasi| quasi.cooked.as_ref())
            .map(|cooked| cooked.to_string_lossy().into_owned())
            .filter(|tail| !tail.is_empty());
        if let Some(tail) = tail {
            return VersionPrefix::UnreadLead(tail);
        }
    }
    VersionPrefix::Unread
}

struct VersioningVisitor {
    imports: HashMap<String, String>,
    found: Vec<AppVersioning>,
}

impl VersioningVisitor {
    fn statement(&self, call: &CallExpr) -> Option<AppVersioning> {
        let Callee::Expr(callee) = &call.callee else {
            return None;
        };
        let Expr::Member(member) = unwrap_expr(callee) else {
            return None;
        };
        if !matches!(&member.prop, MemberProp::Ident(id) if &*id.sym == "enableVersioning") {
            return None;
        }
        let [arg] = call.args.as_slice() else {
            return None;
        };
        let Expr::Object(options) = unwrap_expr(&arg.expr) else {
            return None;
        };
        // `type: <V>.URI`, with `V` an import; any other type states nothing.
        let Ok(Some(kind)) = object_property(options, "type") else {
            return None;
        };
        let Expr::Member(kind) = unwrap_expr(kind) else {
            return None;
        };
        if !matches!(&kind.prop, MemberProp::Ident(id) if &*id.sym == "URI") {
            return None;
        }
        let Expr::Ident(owner) = unwrap_expr(&kind.obj) else {
            return None;
        };
        let type_origin = self.imports.get(&owner.sym.to_string())?.clone();
        let default_version = match object_property(options, "defaultVersion") {
            Ok(None) => RawVersion::Absent,
            Ok(Some(value)) => read_version(value, &self.imports),
            Err(()) => RawVersion::Unread,
        };
        let prefix = match object_property(options, "prefix") {
            Ok(None) => VersionPrefix::Default,
            Ok(Some(value)) => read_prefix(value),
            Err(()) => VersionPrefix::Unread,
        };
        Some(AppVersioning {
            type_origin,
            default_version,
            prefix,
        })
    }
}

impl Visit for VersioningVisitor {
    fn visit_call_expr(&mut self, node: &CallExpr) {
        node.visit_children_with(self);
        if let Some(statement) = self.statement(node) {
            self.found.push(statement);
        }
    }
}

/// The URI versioning statements in one file.
pub fn app_versioning(file_path: &Path, content: &str) -> Vec<AppVersioning> {
    if !content.contains("enableVersioning") {
        return Vec::new();
    }
    let Some((_, module)) = parse_standalone_module(file_path, content) else {
        return Vec::new();
    };
    let mut visitor = VersioningVisitor {
        imports: collect_import_locals(&module),
        found: Vec::new(),
    };
    module.visit_with(&mut visitor);
    visitor.found
}

/// The statements of a service, reduced to the one it states. A statement
/// counts only when its `type` comes from a module the service's verb
/// decorators come from; two that differ state nothing.
pub fn resolve(statements: &[AppVersioning], verb_origins: &HashSet<String>) -> Resolution {
    let mut distinct: Vec<&AppVersioning> = Vec::new();
    for statement in statements
        .iter()
        .filter(|statement| verb_origins.contains(&statement.type_origin))
    {
        if !distinct.contains(&statement) {
            distinct.push(statement);
        }
    }
    let mut resolution = Resolution::default();
    let statement = match distinct.as_slice() {
        [] => return resolution,
        [one] => *one,
        _ => {
            resolution.conflicts = 1;
            return resolution;
        }
    };
    let prefix = match &statement.prefix {
        VersionPrefix::Default => DEFAULT_PREFIX.to_string(),
        VersionPrefix::NoPrefix => String::new(),
        VersionPrefix::Literal(text) => text.clone(),
        VersionPrefix::UnreadLead(tail) => {
            resolution.prefix_unread_lead = 1;
            tail.clone()
        }
        VersionPrefix::Unread => {
            resolution.prefix_unread = 1;
            return resolution;
        }
    };
    let default_version = match &statement.default_version {
        RawVersion::Absent => DefaultVersion::None,
        RawVersion::Literal(text) => DefaultVersion::Literal(text.clone()),
        RawVersion::Neutral { origin } if verb_origins.contains(origin) => DefaultVersion::None,
        RawVersion::Neutral { .. } | RawVersion::Unread => DefaultVersion::Unread,
    };
    resolution.versioning = Some(ServiceVersioning {
        default_version,
        prefix,
    });
    resolution
}

/// The path a route is served under, and whether its version was stated but
/// unread (the path is then left as it was).
pub fn versioned_path(
    service: &ServiceVersioning,
    version: &RouteVersion,
    path: &str,
) -> (String, bool) {
    let stated = match (version, &service.default_version) {
        (RouteVersion::Literal(text), _) => Some(text),
        (RouteVersion::Neutral, _) => None,
        (RouteVersion::Unread, _) | (RouteVersion::Inherit, DefaultVersion::Unread) => {
            return (path.to_string(), true);
        }
        (RouteVersion::Inherit, DefaultVersion::Literal(text)) => Some(text),
        (RouteVersion::Inherit, DefaultVersion::None) => None,
    };
    match stated {
        None => (path.to_string(), false),
        Some(version) if path == "/" => (format!("/{}{version}", service.prefix), false),
        Some(version) => (format!("/{}{version}{path}", service.prefix), false),
    }
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
        let mut routes = SwcScanner::new()
            .decorator_routes(&PathBuf::from("c.ts"), content)
            .routes;
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
