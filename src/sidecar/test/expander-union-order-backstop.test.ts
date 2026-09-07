/**
 * The canonical union order must hold at the DEPTH BACKSTOP too (carrick#775).
 *
 * `expandTypeStructural` stops recursing past `MAX_EXPANSION_DEPTH` and hands
 * back `namedText(type)` — the compiler's own print. For a union that print is
 * in type-id creation order, which is the instability #735 removed from the
 * expanded path and left untouched here: on the trig-bench webapp blob 12 of
 * the 40 literal unions in the index sit at brace depth 12 to 17, and every one
 * of them still printed in creation order after #761.
 *
 * The perturbation is the same as `expander-union-order.test.ts`: one source,
 * two aliases spelling the same literal set in different orders, resolved in
 * the two possible orders over two fresh programs. Whichever declaration the
 * checker reaches first decides the interned literals' ids, so a print that
 * depends on those ids differs between the two runs.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import { Project, type Type } from 'ts-morph';
import {
  expandTypeStructural,
  MAX_EXPANSION_DEPTH,
} from '../src/type-structural-expander.js';

const MEMBERS_JOB = `'PENDING' | 'TIMED_OUT' | 'CANCELED' | 'RUNNING'`;
const MEMBERS_RUN = `'CANCELED' | 'RUNNING' | 'TIMED_OUT' | 'PENDING'`;

/**
 * Levels of object nesting between the alias and the union. The union is the
 * property type of the innermost object, so it is expanded at depth
 * `NESTING + 1` — one past the backstop when `NESTING === MAX_EXPANSION_DEPTH`.
 */
const NESTING = MAX_EXPANSION_DEPTH;

/**
 * `interface <name>0 { status: <members>; payload: Marker }` wrapped in
 * `NESTING` outer objects. `payload` is the fixture guard: `Marker` has
 * members, so it prints expanded everywhere ABOVE the backstop and by name at
 * it — see the guard case below.
 */
function nested(name: string, members: string): string {
  const lines = [
    `interface ${name}0 { status: ${members}; payload: Marker }`,
  ];
  for (let i = 1; i <= NESTING; i++) {
    lines.push(`interface ${name}${i} { next: ${name}${i - 1} }`);
  }
  lines.push(`export type ${name} = ${name}${NESTING};`);
  return lines.join('\n');
}

const SOURCE = [
  'interface Marker { tag: string; note: string }',
  nested('Job', MEMBERS_JOB),
  nested('Run', MEMBERS_RUN),
].join('\n');

/** Expand the named aliases in the given order, over a fresh program. */
function expandInOrder(aliases: string[]): Map<string, string> {
  const project = new Project({
    useInMemoryFileSystem: true,
    compilerOptions: { strict: true },
  });
  const sf = project.createSourceFile('surface.ts', SOURCE);
  const out = new Map<string, string>();
  for (const alias of aliases) {
    out.set(alias, expandTypeStructural(sf.getTypeAliasOrThrow(alias).getType()));
  }
  return out;
}

/** A fresh program's type for one alias, for the direct-depth cases. */
function aliasType(alias: string, source = SOURCE): Type {
  const project = new Project({
    useInMemoryFileSystem: true,
    compilerOptions: { strict: true },
  });
  return project
    .createSourceFile('surface.ts', source)
    .getTypeAliasOrThrow(alias)
    .getType();
}

describe('expandTypeStructural union order at the depth backstop (#775)', () => {
  it('puts the fixture union past MAX_EXPANSION_DEPTH', () => {
    // Without this the two cases below would exercise the shallow path, which
    // #761 already made canonical, and would pass whatever the backstop does.
    assert.ok(
      NESTING + 1 > MAX_EXPANSION_DEPTH,
      `the union sits at depth ${NESTING + 1}, not past ${MAX_EXPANSION_DEPTH}`,
    );
    const expanded = expandInOrder(['Job']).get('Job')!;
    // `Marker` is a sibling property of the union at the same depth. Above the
    // backstop it would print as `{ tag: string; note: string; }`; printing by
    // name is what proves the fixture reaches the backstop.
    assert.ok(
      /payload: Marker\b/.test(expanded),
      `fixture no longer reaches the backstop — payload printed expanded:\n${expanded}`,
    );
  });

  it('prints a backstopped union byte-identically under a perturbed creation order', () => {
    const jobFirst = expandInOrder(['Job', 'Run']);
    const runFirst = expandInOrder(['Run', 'Job']);

    for (const alias of ['Job', 'Run']) {
      assert.strictEqual(
        jobFirst.get(alias),
        runFirst.get(alias),
        `${alias} must print identically whichever alias is resolved first`,
      );
    }
  });

  it('keeps every member of a backstopped union', () => {
    const expanded = expandInOrder(['Job']).get('Job')!;
    for (const member of ['"PENDING"', '"TIMED_OUT"', '"CANCELED"', '"RUNNING"']) {
      assert.ok(
        expanded.includes(member),
        `${member} missing from the backstopped union:\n${expanded}`,
      );
    }
  });

  it('sorts the members of a union handed straight to the backstop', () => {
    // The backstop reached directly, with no nesting in the way: the same
    // canonical order the expanded path produces.
    const union = aliasType('Status', `export type Status = ${MEMBERS_JOB};`);
    assert.strictEqual(
      expandTypeStructural(union, new Set(), MAX_EXPANSION_DEPTH + 1),
      '"CANCELED" | "PENDING" | "RUNNING" | "TIMED_OUT"',
    );
  });

  it('prints a mixed union the same way at the backstop as above it', () => {
    // One rule for every member, and the two paths agree — the backstop is a
    // depth bound on recursion, not a second normal form.
    const union = aliasType(
      'Maybe',
      `export type Maybe = 'b' | null | 'a' | number;`,
    );
    const atBackstop = expandTypeStructural(
      union,
      new Set(),
      MAX_EXPANSION_DEPTH + 1,
    );
    const aboveIt = expandTypeStructural(union, new Set(), 0);
    assert.strictEqual(atBackstop, aboveIt);
    assert.strictEqual(atBackstop, '"a" | "b" | null | number');
  });

  it('leaves a non-union backstop print alone', () => {
    const marker = aliasType('Marker2', 'export type Marker2 = { a: string };');
    assert.strictEqual(
      expandTypeStructural(marker, new Set(), MAX_EXPANSION_DEPTH + 1),
      '{ a: string; }',
    );
  });
});
