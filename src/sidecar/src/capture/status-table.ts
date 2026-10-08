/**
 * Response tables keyed by HTTP status code (carrick#1841).
 *
 * An object type whose every key is a status code (`{ 200: Item; 404: Problem
 * }`, or a range key such as `2XX`) is not a body: it is a table of the bodies
 * each status carries. What a caller receives on success is the union of its
 * 2xx values. The producer side reads the same rule off a schema object
 * literal (`successStatusEntry` in the inferrer); this module reads it off a
 * TYPE, through the compiler's own API, so the inferrer, the v1 bundle and
 * the capture all decide a table the same way.
 *
 * Nothing here names a library or a generator. The shape is the HTTP status
 * vocabulary, which is the language of the wire, not of any one client.
 */

import ts from 'typescript';

/** `value` when it is an integer in the HTTP status range, else `undefined`. */
export function httpStatus(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isInteger(value) && value >= 100 && value <= 599
    ? value
    : undefined;
}

/** One verdict for a set of status codes, or `mixed` when they disagree. */
export function classifyStatusCodes(
  codes: number[]
): 'success' | 'error' | 'redirect' | 'mixed' {
  if (codes.every((code) => code >= 400)) return 'error';
  if (codes.every((code) => code >= 300 && code < 400)) return 'redirect';
  if (codes.every((code) => code < 300)) return 'success';
  return 'mixed';
}

/**
 * The status code a response-table KEY names: `200`, or a range key (`2XX`,
 * `2xx`) read as the bottom of its range. Quotes around the key are ignored.
 * `undefined` for any other key.
 */
export function statusKeyCode(name: string): number | undefined {
  const key = name.replace(/['"`]/g, '').trim();
  if (/^\d{3}$/.test(key)) return httpStatus(Number(key));
  if (/^[1-5]xx$/i.test(key)) return Number(key[0]) * 100;
  return undefined;
}

/** One row of a response table: the key as written and the status it names. */
export interface StatusTableEntry {
  symbol: ts.Symbol;
  /** The key as the type declares it (`200`, `2XX`). */
  key: string;
  code: number;
}

/**
 * The rows of `type` when it is a response table: a plain object type with at
 * least one property, every key a status code, no index signature and no call
 * or construct signature. `undefined` for anything else, so a payload that
 * happens to hold one numeric key among others is never read as a table.
 */
export function statusTableOf(
  checker: ts.TypeChecker,
  type: ts.Type
): StatusTableEntry[] | undefined {
  if (!(type.flags & ts.TypeFlags.Object)) return undefined;
  if (type.getCallSignatures().length > 0 || type.getConstructSignatures().length > 0) {
    return undefined;
  }
  if (checker.getIndexInfosOfType(type).length > 0) return undefined;
  const properties = checker.getPropertiesOfType(type);
  if (properties.length === 0) return undefined;
  const entries: StatusTableEntry[] = [];
  for (const symbol of properties) {
    const key = symbol.getName();
    const code = statusKeyCode(key);
    if (code === undefined) return undefined;
    entries.push({ symbol, key, code });
  }
  return entries;
}

/** The rows of a table that answer a success: 2xx, in the order declared. */
export function successEntries(entries: StatusTableEntry[]): StatusTableEntry[] {
  return entries.filter((entry) => entry.code >= 200 && entry.code <= 299);
}

/**
 * Whether a body type states nothing: `void`, `undefined`, `never`, or a union
 * made only of them. A `204: void` row is such a body.
 */
export function statesNoBody(type: ts.Type): boolean {
  const members = type.isUnion() ? type.types : [type];
  return members.every(
    (member) =>
      (member.flags & (ts.TypeFlags.Void | ts.TypeFlags.Undefined | ts.TypeFlags.Never)) !== 0
  );
}

/**
 * What a response table's success rows carry, read for a consumer:
 *  - `body`: the 2xx rows' value types, at least one of which states a body;
 *  - `no_body`: the table has no 2xx row, or every 2xx row states nothing.
 * `undefined` when `type` is not a response table.
 */
export function statusTableBody(
  checker: ts.TypeChecker,
  type: ts.Type,
  at: ts.Node
):
  | { kind: 'body'; entries: StatusTableEntry[]; types: ts.Type[] }
  | { kind: 'no_body'; entries: StatusTableEntry[] }
  | undefined {
  const entries = statusTableOf(checker, type);
  if (!entries) return undefined;
  const success = successEntries(entries);
  const types = success.map((entry) => checker.getTypeOfSymbolAtLocation(entry.symbol, at));
  if (success.length === 0 || types.every((body) => statesNoBody(body))) {
    return { kind: 'no_body', entries };
  }
  return { kind: 'body', entries: success, types };
}

/** The members of a union, or the type itself: what a set comparison reads. */
export function constituents(type: ts.Type): ts.Type[] {
  return type.isUnion() ? type.types : [type];
}
