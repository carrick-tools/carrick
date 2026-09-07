/**
 * The text-level canonical union order (carrick#775, #760).
 *
 * `canonicalizeUnionsInText` is the half of the normal form that works on the
 * compiler's own print, where the type API can no longer reach the members.
 * Two properties matter and are asserted separately:
 *
 *  - unions come out in the same canonical order `orderMembers` produces
 *    (intrinsics first in printed order, then members sorted by text);
 *  - EVERYTHING ELSE comes back byte-identical. A type print is arbitrary
 *    text and this rewrite is not a parser, so the cases it does not
 *    understand — function types, conditionals, unbalanced prints — must be
 *    returned untouched rather than half-rewritten, and order-significant
 *    lists (tuples, generic arguments) must never be sorted.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import { canonicalizeUnionsInText } from '../src/type-text-canonicalizer.js';

describe('canonicalizeUnionsInText (#775)', () => {
  it('orders a union at the top level', () => {
    assert.strictEqual(
      canonicalizeUnionsInText('"sum" | "count" | "min"'),
      '"count" | "min" | "sum"',
    );
  });

  it('orders a union nested inside a generic argument', () => {
    // The live shape: a library type kept by name, whose whole instantiation
    // rides along in the compiler's print.
    assert.strictEqual(
      canonicalizeUnionsInText('Enum<{ code: "b" | "a"; raw: string; }>'),
      'Enum<{ code: "a" | "b"; raw: string; }>',
    );
  });

  it('orders a union inside an index-signature body', () => {
    // The other live shape: an object with no named properties for the walk
    // to descend into, so the whole subtree is one compiler print.
    assert.strictEqual(
      canonicalizeUnionsInText('{ [x: string]: "z" | "a"; }'),
      '{ [x: string]: "a" | "z"; }',
    );
  });

  it('sorts an intrinsic like any other member', () => {
    // One rule for every member: quoted literals sort ahead of bare names
    // because `"` precedes a letter in UTF-16, and that is all the rule says.
    // `orderMembers` on the type-API side does exactly the same, so a union
    // cannot print two ways depending on which path rendered it.
    assert.strictEqual(
      canonicalizeUnionsInText('"b" | null | "a" | undefined'),
      '"a" | "b" | null | undefined',
    );
  });

  it('returns text with nothing to reorder byte-identically', () => {
    for (const text of [
      '{ id: string; total: number; }',
      'Array<{ a: string; }>',
      '"a" | "b" | "c"',
      'Record<string, string> | undefined',
      '{  odd   spacing : string ;  }',
    ]) {
      assert.strictEqual(canonicalizeUnionsInText(text), text);
    }
  });

  it('fronts an intrinsic the compiler printed last', () => {
    // The compiler prints an optional member's type as `T | undefined`; the
    // type-API path (`orderMembers`) prints `undefined | T`, because
    // intrinsics lead. The text path has to agree, or one union prints two
    // ways depending on which side of a `namedText` boundary it fell.
    assert.strictEqual(
      canonicalizeUnionsInText('Record<string, string> | undefined'),
      'Record<string, string> | undefined',
    );
  });

  it('never splits on a pipe inside a string literal', () => {
    const text = '{ pattern: "a|b"; mode: "y" | "x"; }';
    assert.strictEqual(
      canonicalizeUnionsInText(text),
      '{ pattern: "a|b"; mode: "x" | "y"; }',
    );
  });

  it('leaves a function type exactly as printed', () => {
    // `() => "b" | "a"` is one function returning a union, not a two-member
    // union: the depth-zero pipe belongs to the return type. Rewriting it
    // would move the arrow.
    for (const text of [
      '() => "b" | "a"',
      '(input: "b" | "a") => void',
      '{ handler: (x: number) => "b" | "a"; mode: "y" | "x"; }',
    ]) {
      const out = canonicalizeUnionsInText(text);
      assert.ok(
        out.startsWith(text.slice(0, text.indexOf('=>'))),
        `the arrow must not move: ${out}`,
      );
    }
    assert.strictEqual(canonicalizeUnionsInText('() => "b" | "a"'), '() => "b" | "a"');
  });

  it('leaves a conditional type exactly as printed', () => {
    const text = 'T extends string ? "b" | "a" : never';
    assert.strictEqual(canonicalizeUnionsInText(text), text);
  });

  it('never reorders a tuple, whose order is meaning', () => {
    const text = '["b", "a", "c"]';
    assert.strictEqual(canonicalizeUnionsInText(text), text);
  });

  it('never reorders generic arguments', () => {
    const text = 'Map<"b", "a">';
    assert.strictEqual(canonicalizeUnionsInText(text), text);
  });

  it('is idempotent', () => {
    const once = canonicalizeUnionsInText('{ [x: string]: "z" | Enum<"b" | "a">; }');
    assert.strictEqual(canonicalizeUnionsInText(once), once);
  });

  it('hands back a print it cannot parse rather than half-rewriting it', () => {
    for (const text of ['{ a: "b" | "a"', 'Enum<"b" | "a"', '', '   ']) {
      assert.strictEqual(canonicalizeUnionsInText(text), text);
    }
  });
});
