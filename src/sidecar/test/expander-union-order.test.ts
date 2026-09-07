/**
 * Union member order must not depend on the order the checker created the
 * member types (carrick#735).
 *
 * A union's constituents are stored sorted by type id, and a literal type is
 * created — and interned — the first time a declaration mentioning it is
 * checked. So on a tree where two declarations spell the same literal set in
 * different orders, whichever one is reached first decides how BOTH print.
 * That is what made two runs of the same binary over an unchanged tree write
 * two different `expanded_definition` strings for one operation.
 *
 * The perturbation here is exactly the live one: the same source, resolved in
 * two different alias orders. Before the fix the two orders print two
 * different strings; after it they are byte-equal.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import { Project } from 'ts-morph';
import { expandTypeStructural } from '../src/type-structural-expander.js';

const SOURCE = `
  interface Job { status: 'PENDING' | 'TIMED_OUT' | 'CANCELED' | 'RUNNING' }
  interface Run { state: 'CANCELED' | 'RUNNING' | 'TIMED_OUT' | 'PENDING' }
  export type SurfaceJob = Job;
  export type SurfaceRun = Run;
`;

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

describe('expandTypeStructural union member order (#735)', () => {
  it('prints the same union byte-identically under a perturbed creation order', () => {
    const jobFirst = expandInOrder(['SurfaceJob', 'SurfaceRun']);
    const runFirst = expandInOrder(['SurfaceRun', 'SurfaceJob']);

    for (const alias of ['SurfaceJob', 'SurfaceRun']) {
      assert.strictEqual(
        jobFirst.get(alias),
        runFirst.get(alias),
        `${alias} must print identically whichever alias is resolved first`,
      );
    }
  });

  it('keeps every union member, only the order is canonical', () => {
    const expanded = expandInOrder(['SurfaceJob']).get('SurfaceJob')!;
    for (const member of ['"PENDING"', '"TIMED_OUT"', '"CANCELED"', '"RUNNING"']) {
      assert.ok(
        expanded.includes(member),
        `member ${member} must survive the canonical order, got: ${expanded}`,
      );
    }
    assert.strictEqual(
      expanded,
      '{ status: "CANCELED" | "PENDING" | "RUNNING" | "TIMED_OUT"; }',
    );
  });

  it('sorts intrinsics by text like every other member', () => {
    const project = new Project({
      useInMemoryFileSystem: true,
      compilerOptions: { strict: true },
    });
    const sf = project.createSourceFile(
      'mixed.ts',
      `
      interface Zebra { z: string }
      export type Mixed = number | null | undefined | Zebra | 'lit';
      `,
    );
    const expanded = expandTypeStructural(sf.getTypeAliasOrThrow('Mixed').getType());
    // This used to keep the intrinsics ahead of the rest in compiler-id order,
    // on the grounds that it reproduced the compiler's own print. It does not:
    // `typeToString` prints `number | null` where the ids say `null | number`.
    // Once carrick#775 put the compiler's prints under the same canonical rule,
    // an id-ordered exception meant one union could print two ways depending on
    // which path rendered it, so there is now one rule for every member.
    assert.strictEqual(
      expanded,
      '"lit" | null | number | undefined | { z: string; }',
    );
  });

  it('orders intersection members canonically too', () => {
    const project = new Project({
      useInMemoryFileSystem: true,
      compilerOptions: { strict: true },
    });
    const sf = project.createSourceFile(
      'inter.ts',
      `
      interface B { b: string }
      interface A { a: string }
      export type ZA = B & A;
      export type AZ = A & B;
      `,
    );
    const za = expandTypeStructural(sf.getTypeAliasOrThrow('ZA').getType());
    const az = expandTypeStructural(sf.getTypeAliasOrThrow('AZ').getType());
    assert.strictEqual(za, az, 'an intersection must print the same either way round');
  });
});
