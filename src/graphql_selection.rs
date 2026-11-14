//! What a GraphQL operation selects, and the fields it selects that the schema
//! it is sent to does not declare.
//!
//! A GraphQL server validates every operation against its schema before it
//! runs it, and a field the schema does not declare on the type it is selected
//! on fails the whole operation (the specification's field selection rule).
//! That is decided by two texts alone, the schema's type definitions and the
//! operation's selection set, so no compiler type takes part. It has to be read
//! here because the type check cannot see it, twice over:
//!
//! - a code-first schema's resolver returns its backing record, which can hold
//!   a member the schema no longer exposes (carrick#1875), so the producer's
//!   type still has the field;
//! - a client's generated result type marks every nullable field optional
//!   (`field?: T | null`), so a producer type without the field still
//!   satisfies it.
//!
//! The answer is a fact only when the schema the scan read is the whole schema
//! the server serves. [`PartialSchema`] says when it may not be (a printed
//! code-first schema is never taken as whole: it can be stale), and a
//! doubt is carried beside the fields rather than dropped: the pair is then
//! not judged compatible either (`Analyzer::set_pair_outcomes`).
//!
//! Fail closed throughout: a parent type the schema does not define as an
//! object or interface type has its direct fields left unread (a union's
//! members are read through its inline fragments), and a fragment spread whose
//! definition the document does not hold is not read.

use std::collections::{BTreeSet, HashMap, HashSet};

use graphql_parser::query;
use graphql_parser::schema;
use serde::{Deserialize, Serialize};

use crate::operation::GraphqlOperationKind;

/// What one field's selection set asks for, as the document states it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelectionSet {
    /// Fields selected directly, by their schema name (an alias names the
    /// result key, not the field).
    pub fields: Vec<SelectedField>,
    /// Selections that apply on a type: inline fragments, and fragment spreads
    /// read through to their definition. `None` is an inline fragment with no
    /// type condition, which applies on the parent type.
    pub on_types: Vec<(Option<String>, SelectionSet)>,
}

/// One selected field and what it selects in turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedField {
    pub name: String,
    pub selection: SelectionSet,
}

/// A field an operation selects on a type whose schema does not declare it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct UndeclaredField {
    pub on_type: String,
    pub field: String,
}

/// Why the schema an operation was read against may not be the whole schema
/// its server serves. Any of these makes the reading a doubt, never a break.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartialSchema {
    /// The repository serves more than one schema file, so a server may
    /// compose several of them, or the operation may go to another one.
    SeveralSchemas,
    /// The schema file is one a service names in `graphqlSchemas`, the
    /// setting for a schema printed from a code-first server (carrick#1099),
    /// or the reading service builds a schema in code, so any schema file it
    /// serves may be a copy printed from that code (carrick#3200). The server builds its schema from code, so a printed file can be stale
    /// against it: a field the code still declares may be missing from the
    /// file.
    PrintedFromCode,
    /// The schema names, extends or roots its operations in a type it does
    /// not define, so the rest of it is written somewhere else.
    TypesDefinedElsewhere,
    /// The schema applies a directive it does not define and the
    /// specification does not either, as one part of a composed (federated or
    /// stitched) schema does.
    ComposedSchema,
    /// The repository's source states schema text for a type the operation
    /// reads, which a server can add to the schema file when it builds the
    /// schema at runtime.
    SchemaTextInSource,
    /// The repository's source files could not be listed, so schema text in
    /// them could not be ruled out.
    SourcesUnread,
}

impl PartialSchema {
    /// The sentence a doubt is stated in, for a pair it leaves unverifiable.
    pub fn sentence(self) -> &'static str {
        match self {
            PartialSchema::SeveralSchemas => {
                "the repository serves more than one GraphQL schema, so the one read may not be \
                 the whole schema this operation is sent to"
            }
            PartialSchema::PrintedFromCode => {
                "the server is code-first, so this file may be a stale export of its schema"
            }
            PartialSchema::TypesDefinedElsewhere => {
                "the schema read refers to types it does not define, so it is not the whole schema"
            }
            PartialSchema::ComposedSchema => {
                "the schema read applies directives it does not define, as one part of a \
                 federated or stitched schema does"
            }
            PartialSchema::SchemaTextInSource => {
                "the repository's source also states schema text for a type this operation \
                 reads, which a server can add when it builds its schema"
            }
            PartialSchema::SourcesUnread => {
                "the repository's source files could not be listed to rule out schema text in them"
            }
        }
    }
}

/// What an operation's row selects that its schema does not declare, carried
/// on the GraphQL call row (`calls[].undeclared_selection`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndeclaredSelection {
    /// The fields, sorted and deduplicated.
    pub fields: Vec<UndeclaredField>,
    /// The repository-relative schema file they were read against.
    pub schema: String,
    /// Why that schema may not be the whole schema served; absent when the
    /// scan holds it whole, which is when the fields are a break.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial: Option<PartialSchema>,
    /// What this operation selects whose declared type a PR may change. Read
    /// on a PR run only and never written to the index
    /// ([`Self::states_something`]).
    #[serde(skip)]
    pub types: SelectedTypes,
}

/// A leaf field an operation selects, with the named type the head schema
/// declares for it (list and non-null markers stripped).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SelectedLeaf {
    pub on_type: String,
    pub field: String,
    pub named: String,
}

/// A selected field whose named type differs between the schema file as the
/// PR's fork point held it and the file the PR leaves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetypedField {
    pub on_type: String,
    pub field: String,
    pub was: String,
    pub now: String,
}

/// The type half of a PR run's reading of one operation row
/// (`SchemaCatalogue::read_selected_types`, then
/// `SchemaCatalogue::lift_type_changes_made_by_pr`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelectedTypes {
    /// Leaf fields the operation selects, as the head schema types them.
    pub leaves: Vec<SelectedLeaf>,
    /// Whether another schema or schema-stating source may also serve the
    /// operation: then a changed type in this file is not the whole answer.
    pub several_schemas: bool,
    /// The selected fields the PR retypes. Empty until the lift finds some.
    pub retyped: Vec<RetypedField>,
}

/// An `undeclared_selection` that states no undeclared field and no retyped
/// field is read-time scratch and is not written to the index.
pub fn states_nothing(selection: &Option<UndeclaredSelection>) -> bool {
    selection.as_ref().is_none_or(|s| !s.states_something())
}

impl UndeclaredSelection {
    /// Whether this row states a fact or a doubt about the operation: an
    /// undeclared field, or a retyped one. A row that holds only the types
    /// read from the head schema states neither.
    pub fn states_something(&self) -> bool {
        !self.fields.is_empty() || !self.types.retyped.is_empty()
    }

    /// The verdict's diagnostic for a pair whose selected fields the PR
    /// retypes in the schema it serves.
    pub fn retyped_sentence(&self) -> String {
        let fields = self
            .types
            .retyped
            .iter()
            .map(|f| {
                format!(
                    "`{}.{}` from `{}` to `{}`",
                    f.on_type, f.field, f.was, f.now
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "This PR changes {fields} in the schema `{}`. The operation selects the field(s) \
             and reads the old type.",
            self.schema
        )
    }

    /// The verdict's diagnostic for a pair this selection breaks.
    pub fn break_sentence(&self) -> String {
        let fields = self
            .fields
            .iter()
            .map(|f| format!("`{}.{}`", f.on_type, f.field))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "The operation selects {fields}, which the schema `{}` does not declare; a GraphQL \
             server rejects the whole operation.",
            self.schema
        )
    }

    /// Why a pair this selection may break stays unverifiable.
    pub fn doubt_sentence(&self, partial: PartialSchema) -> String {
        let fields = self
            .fields
            .iter()
            .map(|f| format!("`{}.{}`", f.on_type, f.field))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "The operation selects {fields}, which the schema `{}` does not declare, but {}.",
            self.schema,
            partial.sentence()
        )
    }
}

/// Fragment definitions of one document, by name.
pub type Fragments<'d> = HashMap<&'d str, &'d query::FragmentDefinition<'d, String>>;

/// Every fragment definition `document` holds.
pub fn fragments_of<'d>(document: &'d query::Document<'d, String>) -> Fragments<'d> {
    document
        .definitions
        .iter()
        .filter_map(|definition| match definition {
            query::Definition::Fragment(fragment) => Some((fragment.name.as_str(), fragment)),
            query::Definition::Operation(_) => None,
        })
        .collect()
}

/// The selection a parsed selection set states, fragment spreads read through
/// to the definitions in `fragments`.
pub fn read_selection_set(
    set: &query::SelectionSet<'_, String>,
    fragments: &Fragments<'_>,
) -> SelectionSet {
    read_set(set, fragments, &mut Vec::new())
}

fn read_set(
    set: &query::SelectionSet<'_, String>,
    fragments: &Fragments<'_>,
    expanding: &mut Vec<String>,
) -> SelectionSet {
    let mut out = SelectionSet::default();
    for item in &set.items {
        match item {
            query::Selection::Field(field) => out.fields.push(SelectedField {
                name: field.name.clone(),
                selection: read_set(&field.selection_set, fragments, expanding),
            }),
            query::Selection::InlineFragment(inline) => {
                let condition = inline
                    .type_condition
                    .as_ref()
                    .map(|query::TypeCondition::On(name)| name.clone());
                out.on_types.push((
                    condition,
                    read_set(&inline.selection_set, fragments, expanding),
                ));
            }
            query::Selection::FragmentSpread(spread) => {
                // A spread the document does not define is not read, and a
                // fragment that spreads itself is read once.
                let Some(fragment) = fragments.get(spread.fragment_name.as_str()) else {
                    continue;
                };
                if expanding.contains(&spread.fragment_name) {
                    continue;
                }
                expanding.push(spread.fragment_name.clone());
                let query::TypeCondition::On(condition) = &fragment.type_condition;
                let selection = read_set(&fragment.selection_set, fragments, expanding);
                expanding.pop();
                out.on_types.push((Some(condition.clone()), selection));
            }
        }
    }
    out
}

/// Scalars every schema has without defining them.
pub(crate) const BUILT_IN_SCALARS: &[&str] = &["Int", "Float", "String", "Boolean", "ID"];

/// Directives the specification defines, which a schema applies without
/// defining them.
const SPECIFIED_DIRECTIVES: &[&str] = &[
    "deprecated",
    "specifiedBy",
    "oneOf",
    "include",
    "skip",
    "defer",
    "stream",
];

/// One schema file's types, as far as selection validity reads them.
#[derive(Debug, Clone, Default)]
pub struct SchemaFile {
    /// Object and interface types, definitions and extensions together: name
    /// -> field name -> the field's named type, list and non-null markers
    /// stripped.
    composite: HashMap<String, HashMap<String, String>>,
    /// Root operation type names, from a `schema` definition or the defaults.
    roots: HashMap<GraphqlOperationKind, String>,
    /// Why this file alone may not be the whole schema, read from its text.
    own_doubt: Option<PartialSchema>,
}

impl SchemaFile {
    /// Read `text` as a schema. `None` when it does not parse as one or states
    /// no type-system definition (an executable document, say).
    pub fn parse(text: &str) -> Option<Self> {
        let document = schema::parse_schema::<String>(text).ok()?;
        let mut file = SchemaFile::default();
        let mut defined: HashSet<String> = BUILT_IN_SCALARS.iter().map(|s| s.to_string()).collect();
        let mut referenced: BTreeSet<String> = BTreeSet::new();
        let mut extended: BTreeSet<String> = BTreeSet::new();
        let mut directives_defined: HashSet<String> = SPECIFIED_DIRECTIVES
            .iter()
            .map(|name| name.to_string())
            .collect();
        let mut directives_applied: BTreeSet<String> = BTreeSet::new();
        let mut schema_definition = false;
        let mut has_type_system = false;

        fn add_fields(
            file: &mut SchemaFile,
            name: &str,
            fields: &[schema::Field<'_, String>],
            referenced: &mut BTreeSet<String>,
            applied: &mut BTreeSet<String>,
        ) {
            let entry = file.composite.entry(name.to_string()).or_default();
            for field in fields {
                let named = named_type(&field.field_type).to_string();
                referenced.insert(named.clone());
                for argument in &field.arguments {
                    referenced.insert(named_type(&argument.value_type).to_string());
                    applied.extend(argument.directives.iter().map(|d| d.name.clone()));
                }
                applied.extend(field.directives.iter().map(|d| d.name.clone()));
                entry.insert(field.name.clone(), named);
            }
        }

        for definition in &document.definitions {
            match definition {
                schema::Definition::SchemaDefinition(definition) => {
                    has_type_system = true;
                    schema_definition = true;
                    directives_applied.extend(definition.directives.iter().map(|d| d.name.clone()));
                    for (kind, name) in [
                        (GraphqlOperationKind::Query, &definition.query),
                        (GraphqlOperationKind::Mutation, &definition.mutation),
                        (GraphqlOperationKind::Subscription, &definition.subscription),
                    ] {
                        if let Some(name) = name {
                            file.roots.insert(kind, name.clone());
                        }
                    }
                }
                schema::Definition::TypeDefinition(definition) => {
                    has_type_system = true;
                    match definition {
                        schema::TypeDefinition::Object(object) => {
                            defined.insert(object.name.clone());
                            referenced.extend(object.implements_interfaces.iter().cloned());
                            directives_applied
                                .extend(object.directives.iter().map(|d| d.name.clone()));
                            add_fields(
                                &mut file,
                                &object.name,
                                &object.fields,
                                &mut referenced,
                                &mut directives_applied,
                            );
                        }
                        schema::TypeDefinition::Interface(interface) => {
                            defined.insert(interface.name.clone());
                            referenced.extend(interface.implements_interfaces.iter().cloned());
                            directives_applied
                                .extend(interface.directives.iter().map(|d| d.name.clone()));
                            add_fields(
                                &mut file,
                                &interface.name,
                                &interface.fields,
                                &mut referenced,
                                &mut directives_applied,
                            );
                        }
                        schema::TypeDefinition::Union(union) => {
                            defined.insert(union.name.clone());
                            referenced.extend(union.types.iter().cloned());
                            directives_applied
                                .extend(union.directives.iter().map(|d| d.name.clone()));
                        }
                        schema::TypeDefinition::Scalar(scalar) => {
                            defined.insert(scalar.name.clone());
                            directives_applied
                                .extend(scalar.directives.iter().map(|d| d.name.clone()));
                        }
                        schema::TypeDefinition::Enum(enumeration) => {
                            defined.insert(enumeration.name.clone());
                            directives_applied
                                .extend(enumeration.directives.iter().map(|d| d.name.clone()));
                        }
                        schema::TypeDefinition::InputObject(input) => {
                            defined.insert(input.name.clone());
                            directives_applied
                                .extend(input.directives.iter().map(|d| d.name.clone()));
                            for field in &input.fields {
                                referenced.insert(named_type(&field.value_type).to_string());
                                directives_applied
                                    .extend(field.directives.iter().map(|d| d.name.clone()));
                            }
                        }
                    }
                }
                schema::Definition::TypeExtension(extension) => {
                    has_type_system = true;
                    match extension {
                        schema::TypeExtension::Object(object) => {
                            extended.insert(object.name.clone());
                            referenced.extend(object.implements_interfaces.iter().cloned());
                            directives_applied
                                .extend(object.directives.iter().map(|d| d.name.clone()));
                            add_fields(
                                &mut file,
                                &object.name,
                                &object.fields,
                                &mut referenced,
                                &mut directives_applied,
                            );
                        }
                        schema::TypeExtension::Interface(interface) => {
                            extended.insert(interface.name.clone());
                            directives_applied
                                .extend(interface.directives.iter().map(|d| d.name.clone()));
                            add_fields(
                                &mut file,
                                &interface.name,
                                &interface.fields,
                                &mut referenced,
                                &mut directives_applied,
                            );
                        }
                        schema::TypeExtension::Union(union) => {
                            extended.insert(union.name.clone());
                            referenced.extend(union.types.iter().cloned());
                        }
                        schema::TypeExtension::Scalar(scalar) => {
                            extended.insert(scalar.name.clone());
                        }
                        schema::TypeExtension::Enum(enumeration) => {
                            extended.insert(enumeration.name.clone());
                        }
                        schema::TypeExtension::InputObject(input) => {
                            extended.insert(input.name.clone());
                            for field in &input.fields {
                                referenced.insert(named_type(&field.value_type).to_string());
                            }
                        }
                    }
                }
                schema::Definition::DirectiveDefinition(directive) => {
                    has_type_system = true;
                    directives_defined.insert(directive.name.clone());
                    for argument in &directive.arguments {
                        referenced.insert(named_type(&argument.value_type).to_string());
                    }
                }
            }
        }
        if !has_type_system {
            return None;
        }
        if !schema_definition {
            for (kind, name) in [
                (GraphqlOperationKind::Query, "Query"),
                (GraphqlOperationKind::Mutation, "Mutation"),
                (GraphqlOperationKind::Subscription, "Subscription"),
            ] {
                if defined.contains(name) || extended.contains(name) {
                    file.roots.insert(kind, name.to_string());
                }
            }
        }
        let roots_defined = file.roots.values().all(|root| defined.contains(root));
        let self_contained = referenced.iter().all(|name| defined.contains(name))
            && extended.iter().all(|name| defined.contains(name))
            && roots_defined;
        file.own_doubt = if !self_contained {
            Some(PartialSchema::TypesDefinedElsewhere)
        } else if directives_applied
            .iter()
            .any(|name| !directives_defined.contains(name))
        {
            Some(PartialSchema::ComposedSchema)
        } else {
            None
        };
        Some(file)
    }

    /// Why this file alone may not be the whole schema, from its own text.
    pub fn own_doubt(&self) -> Option<PartialSchema> {
        self.own_doubt
    }

    /// The type names of every object and interface type the file defines or
    /// extends, and its root type names.
    pub fn type_names(&self) -> impl Iterator<Item = &str> {
        self.composite
            .keys()
            .map(String::as_str)
            .chain(self.roots.values().map(String::as_str))
    }

    /// The name of the `kind` root type, when this file has one.
    pub fn root_type(&self, kind: GraphqlOperationKind) -> Option<&str> {
        self.roots.get(&kind).map(String::as_str)
    }

    /// The named type of root field `field` of the `kind` root type, when this
    /// file declares that field.
    pub fn root_field_type(&self, kind: GraphqlOperationKind, field: &str) -> Option<&str> {
        let root = self.roots.get(&kind)?;
        self.composite.get(root)?.get(field).map(String::as_str)
    }

    /// The named type this file declares for `field` on the object or
    /// interface type `on_type`, list and non-null markers stripped.
    pub fn field_type(&self, on_type: &str, field: &str) -> Option<&str> {
        self.composite.get(on_type)?.get(field).map(String::as_str)
    }

    /// Whether `name` is a type this file reads as a leaf: not an object or
    /// interface type it defines. Only meaningful when the file is
    /// self-contained (`own_doubt` is `None`), since a type defined elsewhere
    /// also reads as a leaf.
    pub fn is_leaf_type(&self, name: &str) -> bool {
        !self.composite.contains_key(name)
    }

    /// The leaf fields `selection`, selected under root field `field` of a
    /// `kind` operation, selects on types this file declares them on, each
    /// with its named type. A file with a doubt of its own reads none.
    pub fn selected_leaves(
        &self,
        kind: GraphqlOperationKind,
        field: &str,
        selection: &SelectionSet,
    ) -> BTreeSet<SelectedLeaf> {
        let mut out = BTreeSet::new();
        if self.own_doubt.is_none()
            && let Some(parent) = self.root_field_type(kind, field)
        {
            self.collect_leaves(parent, selection, &mut out);
        }
        out
    }

    fn collect_leaves(
        &self,
        parent: &str,
        selection: &SelectionSet,
        out: &mut BTreeSet<SelectedLeaf>,
    ) {
        if let Some(fields) = self.composite.get(parent) {
            for field in &selection.fields {
                if field.name.starts_with("__") {
                    continue;
                }
                let Some(named) = fields.get(&field.name) else {
                    continue;
                };
                if self.is_leaf_type(named) {
                    out.insert(SelectedLeaf {
                        on_type: parent.to_string(),
                        field: field.name.clone(),
                        named: named.clone(),
                    });
                } else {
                    self.collect_leaves(named, &field.selection, out);
                }
            }
        }
        for (condition, nested) in &selection.on_types {
            self.collect_leaves(condition.as_deref().unwrap_or(parent), nested, out);
        }
    }

    /// Whether this file declares `field` on the object or interface type
    /// `on_type`.
    pub fn declares(&self, on_type: &str, field: &str) -> bool {
        self.composite
            .get(on_type)
            .is_some_and(|fields| fields.contains_key(field))
    }

    /// The fields `selection`, selected under root field `field` of a `kind`
    /// operation, selects on a type this file does not declare them on. Empty
    /// when the file does not declare the root field: an operation the schema
    /// does not serve is the missing-operation reading's to state.
    pub fn undeclared(
        &self,
        kind: GraphqlOperationKind,
        field: &str,
        selection: &SelectionSet,
    ) -> BTreeSet<UndeclaredField> {
        let mut out = BTreeSet::new();
        if let Some(parent) = self.root_field_type(kind, field) {
            self.walk(parent, selection, &mut out);
        }
        out
    }

    fn walk(&self, parent: &str, selection: &SelectionSet, out: &mut BTreeSet<UndeclaredField>) {
        if let Some(fields) = self.composite.get(parent) {
            for field in &selection.fields {
                // Introspection fields (`__typename`) are on every type.
                if field.name.starts_with("__") {
                    continue;
                }
                match fields.get(&field.name) {
                    Some(named) => self.walk(named, &field.selection, out),
                    None => {
                        out.insert(UndeclaredField {
                            on_type: parent.to_string(),
                            field: field.name.clone(),
                        });
                    }
                }
            }
        }
        for (condition, nested) in &selection.on_types {
            self.walk(condition.as_deref().unwrap_or(parent), nested, out);
        }
    }
}

fn named_type<'t>(ty: &'t schema::Type<'_, String>) -> &'t str {
    match ty {
        schema::Type::NamedType(name) => name,
        schema::Type::ListType(inner) | schema::Type::NonNullType(inner) => named_type(inner),
    }
}

/// The object and interface type names schema `text` defines or extends,
/// empty when it is not schema text.
pub fn schema_text_types(text: &str) -> BTreeSet<String> {
    SchemaFile::parse(text)
        .map(|file| file.type_names().map(str::to_string).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = r#"
        type Query {
          parcel(id: ID!): Parcel
          depots: [Depot!]!
          stop(id: ID!): Stop
        }
        interface Located { city: String }
        type Parcel implements Located {
          id: ID!
          weightGrams: Int
          city: String
          depot: Depot
        }
        type Depot implements Located { code: String!, city: String }
        union Stop = Parcel | Depot
    "#;

    fn selection_of(document: &str, field: &str) -> SelectionSet {
        let document = query::parse_query::<String>(document).expect("document");
        let fragments = fragments_of(&document);
        for definition in &document.definitions {
            let query::Definition::Operation(query::OperationDefinition::Query(q)) = definition
            else {
                continue;
            };
            for item in &q.selection_set.items {
                if let query::Selection::Field(f) = item
                    && f.name == field
                {
                    return read_selection_set(&f.selection_set, &fragments);
                }
            }
        }
        panic!("no root field {field}");
    }

    fn undeclared(document: &str, field: &str) -> Vec<(String, String)> {
        SchemaFile::parse(SCHEMA)
            .expect("schema")
            .undeclared(
                GraphqlOperationKind::Query,
                field,
                &selection_of(document, field),
            )
            .into_iter()
            .map(|f| (f.on_type, f.field))
            .collect()
    }

    #[test]
    fn a_field_the_type_does_not_declare_is_named_with_its_type() {
        let found = undeclared(
            "query P { parcel(id: 1) { id etaDays depot { code openedAt } } }",
            "parcel",
        );
        assert_eq!(
            found,
            vec![
                ("Depot".to_string(), "openedAt".to_string()),
                ("Parcel".to_string(), "etaDays".to_string()),
            ]
        );
    }

    #[test]
    fn selected_leaves_name_each_selected_scalar_with_its_named_type() {
        let file = SchemaFile::parse(SCHEMA).expect("schema");
        let selection = selection_of(
            "query P { parcel(id: 1) { id weightGrams depot { code } } }",
            "parcel",
        );
        let leaves: Vec<(String, String, String)> = file
            .selected_leaves(GraphqlOperationKind::Query, "parcel", &selection)
            .into_iter()
            .map(|l| (l.on_type, l.field, l.named))
            .collect();
        let leaf = |t: &str, f: &str, n: &str| (t.to_string(), f.to_string(), n.to_string());
        assert_eq!(
            leaves,
            vec![
                leaf("Depot", "code", "String"),
                leaf("Parcel", "id", "ID"),
                leaf("Parcel", "weightGrams", "Int"),
            ]
        );
        assert_eq!(file.field_type("Parcel", "weightGrams"), Some("Int"));
        assert!(file.is_leaf_type("Int"));
        assert!(!file.is_leaf_type("Depot"));
    }

    #[test]
    fn declared_fields_aliases_and_introspection_read_clean() {
        assert!(
            undeclared(
                "query P { parcel(id: 1) { __typename id grams: weightGrams depot { code } } }",
                "parcel"
            )
            .is_empty()
        );
    }

    #[test]
    fn fragments_are_read_on_their_own_type() {
        let document = "query P { stop(id: 1) { ...on Parcel { id etaDays } ...DepotBits } }
                        fragment DepotBits on Depot { code etaDays }";
        assert_eq!(
            undeclared(document, "stop"),
            vec![
                ("Depot".to_string(), "etaDays".to_string()),
                ("Parcel".to_string(), "etaDays".to_string()),
            ]
        );
    }

    #[test]
    fn a_union_field_and_an_unknown_spread_are_not_read() {
        // `city` directly on the union is not read; neither is a spread the
        // document does not define.
        assert!(undeclared("query P { stop(id: 1) { city ...Elsewhere } }", "stop").is_empty());
    }

    #[test]
    fn an_operation_the_schema_does_not_serve_states_nothing_here() {
        assert!(undeclared("query P { parcels { id nope } }", "parcels").is_empty());
    }

    #[test]
    fn interface_fields_are_read_on_the_interface() {
        let schema = "type Query { place: Located } interface Located { city: String }";
        let file = SchemaFile::parse(schema).expect("schema");
        let found = file.undeclared(
            GraphqlOperationKind::Query,
            "place",
            &selection_of("query P { place { city code } }", "place"),
        );
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn a_self_contained_schema_raises_no_doubt() {
        assert_eq!(SchemaFile::parse(SCHEMA).unwrap().own_doubt(), None);
    }

    #[test]
    fn a_schema_that_names_or_extends_types_it_does_not_define_is_partial() {
        for text in [
            "type Query { parcel: Parcel }",
            "type Query { ok: Boolean } extend type Parcel { etaDays: Int }",
            "extend type Query { parcel: Boolean }",
        ] {
            assert_eq!(
                SchemaFile::parse(text).unwrap().own_doubt(),
                Some(PartialSchema::TypesDefinedElsewhere),
                "{text}"
            );
        }
    }

    #[test]
    fn an_undefined_directive_marks_a_composed_schema() {
        let text = "type Query { parcel: Parcel } type Parcel @key(fields: \"id\") { id: ID! }";
        assert_eq!(
            SchemaFile::parse(text).unwrap().own_doubt(),
            Some(PartialSchema::ComposedSchema)
        );
        let defined = "directive @cost(weight: Int) on FIELD_DEFINITION
                       type Query { parcel: Parcel @cost(weight: 2) @deprecated }
                       type Parcel { id: ID! }";
        assert_eq!(SchemaFile::parse(defined).unwrap().own_doubt(), None);
    }

    #[test]
    fn an_executable_document_is_not_a_schema() {
        assert!(SchemaFile::parse("query P { parcel { id } }").is_none());
        assert!(schema_text_types("query P { parcel { id } }").is_empty());
        assert_eq!(
            schema_text_types("extend type Parcel { etaDays: Int }"),
            BTreeSet::from(["Parcel".to_string()])
        );
    }
}
