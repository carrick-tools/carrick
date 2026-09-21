/**
 * carrick#371 mirror-drift guard.
 *
 * The machinery-indicator set is intentionally DUPLICATED across the capture
 * seam: `type-inferrer.ts` (ts-morph, the v1 abstain path) and
 * `capture/machinery.ts` (raw `ts`, the capture demote path) each carry their
 * own copy, because the seam forbids sharing a module across it — a capture/
 * file may import only node builtins + `typescript` + its own bundle, and the
 * rest of the sidecar may reach the bundle only via `api.js`/`index.js`
 * (enforced by capture-v2-seam.test.ts). De-duplicating into a shared module
 * would break that boundary, so the two copies are kept in lockstep by THIS
 * test instead: if they drift, one detection path silently stops abstaining and
 * the carrick#371 false verdict can reappear on whichever path lost a member.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import ts from 'typescript';
import { MACHINERY_MEMBER_INDICATORS as INFERRER_SET } from '../src/type-inferrer.js';
import { isExternalOrigin as inferrerIsExternalOrigin } from '../src/origin.js';
import {
  MACHINERY_MEMBER_INDICATORS as CAPTURE_SET,
  isExternalOrigin as captureIsExternalOrigin,
} from '../src/capture/machinery.js';

describe('carrick#371 machinery-indicator mirror stays in lockstep', () => {
  it('the two duplicated indicator sets are byte-for-byte equal', () => {
    const inferrer = [...INFERRER_SET].sort();
    const capture = [...CAPTURE_SET].sort();
    assert.deepStrictEqual(
      capture,
      inferrer,
      'type-inferrer.ts and capture/machinery.ts MACHINERY_MEMBER_INDICATORS ' +
        'have drifted; update both copies together (see the doc comments)'
    );
  });

  it('the set is non-empty (a truncated copy must not read as "in sync")', () => {
    assert.ok(INFERRER_SET.size >= 3, 'indicator set unexpectedly small');
  });

  it('the two origin gates answer the same for every origin shape', () => {
    // The gate is the other half of the detection: indicators alone never
    // fire. It is asked on a REAL program, because two of its answers come
    // from the compiler rather than from the path: the default libraries, and
    // the files the RESOLVER marked as external-library imports. A Deno host
    // marks an npm dependency's types that way from the module graph while
    // the path carries no `node_modules` segment (carrick#1264), and a
    // workspace package reached through a `node_modules` symlink is marked
    // the same way by the compiler but is the user's own source, so the
    // checkout (the nearest `.git`) is what separates the two.
    const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-origin-gate-')));
    const cache = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-origin-cache-')));
    try {
      const write = (rel: string, text: string): string => {
        const abs = path.join(rel.startsWith('cache/') ? cache : root, rel.replace(/^cache\//, ''));
        fs.mkdirSync(path.dirname(abs), { recursive: true });
        fs.writeFileSync(abs, text);
        return abs;
      };
      fs.mkdirSync(path.join(root, '.git'));
      const user = write(
        'src/routes/orders.ts',
        [
          'import type { Installed } from "installed";',
          'import type { Shared } from "@acme/shared";',
          'import type { Cached } from "cached";',
          'export const order: [Installed, Shared, Cached, Materialised] = null!;',
        ].join('\n')
      );
      const installed = write('node_modules/installed/index.d.ts', 'export interface Installed { a: 1 }');
      const shared = write('packages/shared/src/index.ts', 'export interface Shared { b: 2 }');
      const cached = write('cache/npm/registry/dep/1.0.0/index.d.ts', 'export interface Cached { c: 3 }');
      const materialised = write('.carrick/deno/a1b2c3d4/runtime.d.ts', 'declare global { interface Materialised { d: 4 } }');
      const options: ts.CompilerOptions = {
        strict: true,
        module: ts.ModuleKind.ESNext,
        moduleResolution: ts.ModuleResolutionKind.Bundler,
        target: ts.ScriptTarget.ES2022,
        types: [],
      };
      const host = ts.createCompilerHost(options);
      // The shape `DenoProject.resolve` hands the compiler: a resolved path
      // with no `node_modules` segment plus the graph's external verdict. The
      // workspace package gets the same verdict the compiler gives a symlink.
      host.resolveModuleNames = (names, from) =>
        names.map((name) => {
          if (name === 'cached') return { resolvedFileName: cached, isExternalLibraryImport: true, extension: ts.Extension.Dts };
          if (name === '@acme/shared') return { resolvedFileName: shared, isExternalLibraryImport: true, extension: ts.Extension.Ts };
          return ts.resolveModuleName(name, from, options, host).resolvedModule;
        });
      const program = ts.createProgram([user, materialised], options, host);
      const expected = new Map<string, boolean>([
        [user, false],
        [installed, true],
        [shared, false],
        [cached, true],
        [materialised, true],
      ]);
      let libs = 0;
      for (const sourceFile of program.getSourceFiles()) {
        const capture = captureIsExternalOrigin(program, sourceFile, root);
        // ts-morph types its compiler objects through its own copy of the
        // TypeScript namespace; the objects are the same shape at runtime.
        const inferrer = inferrerIsExternalOrigin(
          program as unknown as Parameters<typeof inferrerIsExternalOrigin>[0],
          sourceFile as unknown as Parameters<typeof inferrerIsExternalOrigin>[1],
          root
        );
        assert.strictEqual(capture, inferrer, `the two origin gates disagree on ${sourceFile.fileName}`);
        const want = expected.get(sourceFile.fileName);
        if (want !== undefined) {
          assert.strictEqual(inferrer, want, `origin gate on ${sourceFile.fileName}`);
          expected.delete(sourceFile.fileName);
        } else {
          assert.ok(program.isSourceFileDefaultLibrary(sourceFile), `unexpected file in program: ${sourceFile.fileName}`);
          assert.strictEqual(inferrer, true, `a default library is machinery origin: ${sourceFile.fileName}`);
          libs += 1;
        }
      }
      assert.deepStrictEqual([...expected.keys()], [], 'every fixture file must be in the program');
      assert.ok(libs > 0, 'the program carries at least one default library');
      // The fixture exercises the compiler clause and nothing else for the
      // cached dependency: no path clause can answer it.
      assert.ok(!cached.includes('/node_modules/') && !cached.includes('/.carrick/deno/'));
      assert.strictEqual(program.isSourceFileFromExternalLibrary(program.getSourceFile(cached)!), true);
      assert.strictEqual(program.isSourceFileFromExternalLibrary(program.getSourceFile(shared)!), true);
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
      fs.rmSync(cache, { recursive: true, force: true });
    }
  });
});
