/**
 * The capture self-check skips type-checking TypeScript's default lib files,
 * and only those (carrick#2158).
 *
 * Checking `lib.d.ts` (DOM included) was most of a capture's self-check time.
 * `skipDefaultLibCheck: true` drops it; `skipLibCheck: false` keeps every
 * stub `.d.ts` checked, which is what makes the gate mean anything.
 *
 * The flag could hide a diagnostic the self-check reads only if the compiler
 * reported it from inside a default lib file. The self-check reads resolution
 * errors (2307/2792, `Cannot find name`), and the place they could move is a
 * stub that augments a global lib interface: the interface's first
 * declaration is in the lib. They stay at the stub.
 *
 * The judge's `tsc` keeps checking the lib: there the flag would drop a
 * stub-located TS2411 the judge counts, so its tsconfig must not carry it.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import ts from 'typescript';
import { SELF_CHECK_OPTIONS } from '../src/capture/self-check.js';
import { CHECKER_TSCONFIG } from '../src/capture/check-workspace.js';

describe('capture self-check: default lib files are not type-checked (carrick#2158)', () => {
  let dir: string;

  before(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-selfcheck-2158-'));
  });
  after(() => {
    fs.rmSync(dir, { recursive: true, force: true });
  });

  it('skips default lib checking and keeps every declaration file checked', () => {
    assert.strictEqual(SELF_CHECK_OPTIONS.skipDefaultLibCheck, true);
    assert.strictEqual(SELF_CHECK_OPTIONS.skipLibCheck, false);
  });

  it('the judge keeps checking the default lib', () => {
    const options = JSON.parse(CHECKER_TSCONFIG).compilerOptions;
    assert.ok(!('skipDefaultLibCheck' in options), 'the judge tsconfig must not skip default lib checks');
  });

  it('resolution errors in a stub that augments lib interfaces stay at the stub', () => {
    const surface = path.join(dir, 'surface.d.ts');
    const augment = path.join(dir, 'augment.d.ts');
    fs.writeFileSync(surface, "export type Body = import('./augment').Shape;\n");
    fs.writeFileSync(
      augment,
      [
        'export interface Shape { id: string }',
        'declare global {',
        "  interface Window { a: import('./gone').T; b: MissingName }",
        '  interface Storage { c: AlsoMissing }',
        '}',
        '',
      ].join('\n')
    );

    const program = ts.createProgram([surface, augment], { ...SELF_CHECK_OPTIONS });
    // The flag has something to skip: the program loaded a default lib that
    // declares the augmented interfaces.
    assert.ok(program.getSourceFiles().some((f) => program.isSourceFileDefaultLibrary(f)));

    const diagnostics = ts.getPreEmitDiagnostics(program);
    const inLib = diagnostics.filter((d) => d.file && program.isSourceFileDefaultLibrary(d.file));
    assert.deepStrictEqual(inLib, []);

    const atAugment = diagnostics
      .filter((d) => d.file && path.resolve(d.file.fileName) === path.resolve(augment))
      .map((d) => `${d.code} ${ts.flattenDiagnosticMessageText(d.messageText, ' ')}`)
      .sort();
    assert.deepStrictEqual(
      atAugment.filter((t) => /^(2304|2307|2792) /.test(t)),
      [
        "2304 Cannot find name 'AlsoMissing'.",
        "2304 Cannot find name 'MissingName'.",
        "2307 Cannot find module './gone' or its corresponding type declarations.",
      ]
    );
  });
});
