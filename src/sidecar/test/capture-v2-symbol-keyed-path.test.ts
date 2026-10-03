/**
 * A member keyed by a unique symbol is named the way the source writes it,
 * the same on every scan (carrick#1766).
 *
 * Such a member has no name of its own. The checker calls it
 * `__@<description>@<symbol id>`, and the id counts every symbol the process
 * made before it, so it differs between two scans of one tree. The deep walk's
 * path reaches the stored record (`any_provenance`) and, through the check
 * phase's pre-gate, a stored verdict's diagnostic, so an id in it made an
 * unchanged tree read as changed.
 *
 * Two programs over the same files stand in for two scans: the compiler
 * numbers symbols per process, so the second program's ids come after the
 * first's.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import ts from 'typescript';
import { findDisqualifyingTopTypes } from '../src/capture/deep-walk.js';

const SOURCE = [
  'export declare const KEY: unique symbol;',
  'export interface Bag {',
  '  id: string;',
  '  [KEY]: any;',
  '  [Symbol.toStringTag]: any;',
  '  inner: { [KEY]: unknown };',
  '}',
  '',
].join('\n');

describe('deep walk names a symbol-keyed member by its key (#1766)', () => {
  let dir: string;

  before(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1766-walk-'));
    fs.writeFileSync(path.join(dir, 'bag.ts'), SOURCE);
  });

  after(() => {
    fs.rmSync(dir, { recursive: true, force: true });
  });

  function walkPaths(): string[] {
    const file = path.join(dir, 'bag.ts');
    const program = ts.createProgram([file], {
      strict: true,
      target: ts.ScriptTarget.ES2022,
      noEmit: true,
    });
    const checker = program.getTypeChecker();
    const source = program.getSourceFile(file)!;
    const bag = source.statements.find(ts.isInterfaceDeclaration)!;
    const type = checker.getTypeAtLocation(bag.name);
    return findDisqualifyingTopTypes(type, program, checker, bag.name).map(
      (finding) => finding.path
    );
  }

  it('writes [KEY] and [Symbol.toStringTag], never the internal id', () => {
    const paths = walkPaths();
    assert.deepStrictEqual(
      [...paths].sort(),
      ['[KEY]', '[Symbol.toStringTag]', 'inner[KEY]'].sort(),
      JSON.stringify(paths)
    );
  });

  it('gives the same paths on two programs', () => {
    const first = walkPaths();
    const second = walkPaths();
    assert.deepStrictEqual(second, first);
    assert.ok(first.every((p) => !p.includes('__@')), JSON.stringify(first));
  });
});
