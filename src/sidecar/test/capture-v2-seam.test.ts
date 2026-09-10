/**
 * Seam enforcement for the v2 capture bundle (pinned decision 11a: "seam,
 * not split" — one cleanly bounded bundle, the stdio action surface is the
 * only contract, zero cross-boundary imports outside it).
 *
 * Mechanically pins:
 *  1. Modules inside src/capture/ import only node builtins, `typescript`,
 *     and each other — never ts-morph, never the legacy sidecar modules.
 *  2. Modules outside the bundle reach it only through the two sanctioned
 *     doors: types from capture/api.js (types.ts) and captureStub from
 *     capture/index.js (the dispatcher).
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
// dist/test -> sidecar root is two levels up; sources live in src/.
const SRC = path.join(__dirname, '..', '..', 'src');
const CAPTURE = path.join(SRC, 'capture');

function importsOf(file: string): string[] {
  // Strip comments first: doc prose legitimately mentions import("...").
  const text = fs
    .readFileSync(file, 'utf8')
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/^\s*\/\/.*$/gm, '');
  const specs: string[] = [];
  const re = /(?:from|import)\s+['"]([^'"]+)['"]/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) specs.push(m[1]);
  // import('...') type references count as boundary crossings too.
  const typeRe = /import\(\s*['"]([^'"]+)['"]\s*\)/g;
  while ((m = typeRe.exec(text)) !== null) specs.push(m[1]);
  // Code-generation templates (`import('${spec}')`) are emitted strings,
  // not imports of this module.
  return specs.filter((s) => !s.includes('${'));
}

function tsFiles(dir: string): string[] {
  return fs
    .readdirSync(dir, { withFileTypes: true })
    .filter((e) => e.isFile() && e.name.endsWith('.ts'))
    .map((e) => path.join(dir, e.name));
}

describe('capture bundle seam (pinned decision 11a)', () => {
  it('capture/ imports only node builtins, typescript, and itself', () => {
    for (const file of tsFiles(CAPTURE)) {
      for (const spec of importsOf(file)) {
        if (spec.startsWith('node:') || spec === 'typescript') continue;
        // A relative specifier must RESOLVE inside capture/: a `./`-prefixed
        // escape like `./../legacy.js` or `./sub/../../x.js` is a boundary
        // violation the bare prefix check waved through (adversarial-review
        // finding 7).
        assert.ok(
          spec.startsWith('./'),
          `${path.basename(file)} imports '${spec}' across the seam`
        );
        const resolved = path.resolve(path.dirname(file), spec);
        assert.ok(
          resolved.startsWith(CAPTURE + path.sep),
          `${path.basename(file)} imports '${spec}', which resolves outside capture/ (${resolved})`
        );
      }
    }
  });

  it('capture/ never uses require() or createRequire (invisible to the import scan)', () => {
    for (const file of tsFiles(CAPTURE)) {
      const text = fs
        .readFileSync(file, 'utf8')
        .replace(/\/\*[\s\S]*?\*\//g, '')
        .replace(/^\s*\/\/.*$/gm, '');
      assert.ok(
        !/\brequire\s*\(/.test(text),
        `${path.basename(file)} calls require() — a seam crossing the import scan cannot see`
      );
      assert.ok(
        !/\bcreateRequire\b/.test(text),
        `${path.basename(file)} uses createRequire — a seam crossing the import scan cannot see`
      );
    }
  });

  it('the rest of the sidecar reaches the bundle only via api.js types and index.js', () => {
    for (const file of tsFiles(SRC)) {
      const base = path.basename(file);
      for (const spec of importsOf(file)) {
        if (!spec.includes('capture/')) continue;
        if (base === 'index.ts' || base === 'project-loader.ts') {
          assert.strictEqual(
            spec,
            './capture/index.js',
            `${base} must use only the capture entry point (shared Deno resolution), saw '${spec}'`
          );
        } else if (base === 'types.ts') {
          assert.strictEqual(
            spec,
            './capture/api.js',
            `types.ts must use only the api.js types door, saw '${spec}'`
          );
        } else {
          assert.fail(`${base} reaches into the capture bundle ('${spec}')`);
        }
      }
    }
  });
});
