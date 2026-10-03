/**
 * A member's place in a path, written the same way on every scan
 * (carrick#1766).
 *
 * A member keyed by a unique symbol (`[Symbol.iterator]`, or a user's
 * `const KEY: unique symbol`) has no name of its own. The checker escapes it as
 * `__@<description>@<symbol id>`, and the id counts the symbols the process
 * made before this one, so it differs between two scans of one tree and a
 * stored sentence naming it changed on every scan. The checker prints such a
 * member from its key instead, `[Symbol.iterator]`, which is how the source
 * writes it.
 *
 * Every other member keeps `getName()`. A string key that starts with `__` is
 * escaped with one more underscore, so it never reads as symbol-keyed here, and
 * `symbolToString` would quote a key such as `content-type`, changing text that
 * is already stable.
 */

import ts from 'typescript';

/** `parent.name`, or `parent[KEY]` for a member keyed by a unique symbol. */
export function memberPath(parent: string, member: ts.Symbol, checker: ts.TypeChecker): string {
  if (!(member.escapedName as string).startsWith('__@')) {
    return parent === '' ? member.getName() : `${parent}.${member.getName()}`;
  }
  return `${parent}${checker.symbolToString(member)}`;
}
