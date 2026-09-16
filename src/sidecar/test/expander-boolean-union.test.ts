/**
 * A boolean prints as `boolean` wherever a served type is rendered
 * (carrick#1165).
 *
 * The compiler stores `boolean` as the union `false | true`. Its own printer
 * folds the pair back, but the structural expander renders union members one
 * by one: an optional `flag?: boolean` has its `undefined` stripped and the two
 * literals left over were joined as `flag?: false | true`. That text is what
 * the index serves, and the capture's literal backfill carries it into the
 * stub. The compiler's print can hold the same pair once a union is rebuilt
 * from members, so the text-level canonicaliser folds it too.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import { Project } from 'ts-morph';
import { expandTypeStructural } from '../src/type-structural-expander.js';
import { canonicalizeUnionsInText } from '../src/type-text-canonicalizer.js';

const SOURCE = `
  interface Signals {
    nameMatch: boolean;
    nameFuzzy?: boolean;
    archived: boolean | null;
    label?: string | boolean;
    exact: true;
    either: 'yes' | false;
  }
  export type Surface = Signals;
`;

function expand(): string {
  const project = new Project({
    useInMemoryFileSystem: true,
    compilerOptions: { strict: true },
  });
  const sf = project.createSourceFile('surface.ts', SOURCE);
  return expandTypeStructural(sf.getTypeAliasOrThrow('Surface').getType());
}

describe('boolean prints as boolean (#1165)', () => {
  it('folds the literal pair left by stripping undefined off an optional member', () => {
    const expanded = expand();
    assert.ok(!/false \| true|true \| false/.test(expanded), expanded);
    assert.match(expanded, /nameFuzzy\?: boolean;/, expanded);
    assert.match(expanded, /nameMatch: boolean;/, expanded);
  });

  it('folds inside a wider union and keeps the canonical member order', () => {
    const expanded = expand();
    assert.match(expanded, /archived: boolean \| null;/, expanded);
    assert.match(expanded, /label\?: boolean \| string;/, expanded);
  });

  it('leaves a lone boolean literal alone', () => {
    const expanded = expand();
    assert.match(expanded, /exact: true;/, expanded);
    assert.match(expanded, /either: "yes" \| false;/, expanded);
  });

  it('folds the pair in compiler-printed text as well', () => {
    assert.strictEqual(
      canonicalizeUnionsInText('{ a?: false | true; b: true | null | false; c: false; }'),
      '{ a?: boolean; b: boolean | null; c: false; }',
    );
    assert.strictEqual(canonicalizeUnionsInText('Array<true | false>'), 'Array<boolean>');
  });
});
