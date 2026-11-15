/**
 * The SDL side of a GraphQL producer response (carrick#4403): what type each
 * field of the response is declared as, read from the service's one type
 * table (`GraphqlTypes`) from the root field down.
 *
 * A GraphQL server sends each leaf through its scalar's `serialize`. The
 * built-in scalars are fixed by the spec (`GRAPHQL_SCALAR_SENT`); a custom
 * scalar's `serialize` is not read, so it keeps the JSON form, and so does an
 * enum. Both the judge's probe and the stored type text read the table only
 * through `graphqlFieldLeaf`, the one lookup.
 *
 * Seam: this bundle only.
 */

import type { GraphqlLeaf, GraphqlRoot, GraphqlTypes } from './api.js';

/**
 * The type each built-in scalar sends, by the GraphQL spec: `String` and `ID`
 * serialise to a string, `Int` and `Float` to a number, `Boolean` to a
 * boolean. Every other scalar keeps the JSON form.
 */
export const GRAPHQL_SCALAR_SENT: Readonly<Record<string, 'string' | 'number' | 'boolean'>> =
  Object.freeze({
    String: 'string',
    ID: 'string',
    Int: 'number',
    Float: 'number',
    Boolean: 'boolean',
  });

/** The type a built-in scalar sends, or `undefined` for any other scalar. */
export function graphqlScalarSent(name: string): 'string' | 'number' | 'boolean' | undefined {
  return Object.prototype.hasOwnProperty.call(GRAPHQL_SCALAR_SENT, name)
    ? GRAPHQL_SCALAR_SENT[name]
    : undefined;
}

/** `leaf` with its non-null wrappers removed; a list is kept. */
export function withoutNonNull(leaf: GraphqlLeaf): GraphqlLeaf {
  let current = leaf;
  while ('non_null' in current) current = current.non_null;
  return current;
}

function leavesEqual(a: GraphqlLeaf, b: GraphqlLeaf): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

/**
 * The declared output type of `field` on `typeName`, or `undefined` when the
 * table does not say. An object or interface answers from its fields. A union
 * answers only for a field every member declares with the same type; a
 * member that is itself a union is read the same way, and a union met again
 * on the way answers nothing, so a cyclic table ends.
 */
export function graphqlFieldLeaf(
  types: GraphqlTypes,
  typeName: string,
  field: string,
  visiting: ReadonlySet<string> = new Set()
): GraphqlLeaf | undefined {
  if (visiting.has(typeName)) return undefined;
  if (!Object.prototype.hasOwnProperty.call(types, typeName)) return undefined;
  const def = types[typeName];
  if ('fields' in def) {
    return Object.prototype.hasOwnProperty.call(def.fields, field)
      ? def.fields[field]
      : undefined;
  }
  if (def.members.length === 0) return undefined;
  const inner = new Set(visiting).add(typeName);
  let shared: GraphqlLeaf | undefined;
  for (const member of def.members) {
    const leaf = graphqlFieldLeaf(types, member, field, inner);
    if (leaf === undefined) return undefined;
    if (shared === undefined) shared = leaf;
    else if (!leavesEqual(shared, leaf)) return undefined;
  }
  return shared;
}

/** The fields `typeName` answers for, as `graphqlFieldLeaf` reads them. */
function graphqlFieldsOf(
  types: GraphqlTypes,
  typeName: string,
  visiting: ReadonlySet<string> = new Set()
): string[] {
  if (visiting.has(typeName)) return [];
  if (!Object.prototype.hasOwnProperty.call(types, typeName)) return [];
  const def = types[typeName];
  if ('fields' in def) return Object.keys(def.fields);
  const inner = new Set(visiting).add(typeName);
  const names = new Set<string>();
  for (const member of def.members) {
    for (const name of graphqlFieldsOf(types, member, inner)) names.add(name);
  }
  return [...names].filter((name) => graphqlFieldLeaf(types, typeName, name) !== undefined);
}

/**
 * Whether the table holds `typeName`. A type no schema file of the service
 * declares (an undeclared custom scalar) is a reference with no entry: a
 * leaf that names it is read as unknown, in the JSON form.
 */
export function graphqlDeclaresType(types: GraphqlTypes, typeName: string): boolean {
  return Object.prototype.hasOwnProperty.call(types, typeName);
}

/** The leaf the root field is declared as, or `undefined` when the table has none. */
export function graphqlRootLeaf(types: GraphqlTypes, root: GraphqlRoot): GraphqlLeaf | undefined {
  return graphqlFieldLeaf(types, root.type, root.field);
}

/** The named type a leaf refers to through its wrappers, if it refers to one. */
function referencedType(leaf: GraphqlLeaf): string | undefined {
  let current = leaf;
  while ('non_null' in current || 'list' in current) {
    current = 'non_null' in current ? current.non_null : current.list;
  }
  return 'type' in current ? current.type : undefined;
}

/**
 * The table a probe needs for one response: every type reachable from
 * `leaf`, each as the fields `graphqlFieldLeaf` answers for it (a union's
 * shared fields), keyed and ordered by name so the probe text is stable.
 */
export function graphqlReachableFields(
  types: GraphqlTypes,
  leaf: GraphqlLeaf
): Record<string, Record<string, GraphqlLeaf>> {
  const out: Record<string, Record<string, GraphqlLeaf>> = {};
  const pending: string[] = [];
  const first = referencedType(leaf);
  if (first !== undefined) pending.push(first);
  const seen = new Set<string>();
  while (pending.length > 0) {
    const name = pending.pop()!;
    if (seen.has(name)) continue;
    seen.add(name);
    if (!graphqlDeclaresType(types, name)) continue;
    const fields: Record<string, GraphqlLeaf> = {};
    for (const field of graphqlFieldsOf(types, name).sort()) {
      const fieldLeaf = graphqlFieldLeaf(types, name, field)!;
      fields[field] = fieldLeaf;
      const next = referencedType(fieldLeaf);
      if (next !== undefined && !seen.has(next)) pending.push(next);
    }
    out[name] = fields;
  }
  return Object.fromEntries(Object.keys(out).sort().map((name) => [name, out[name]]));
}
