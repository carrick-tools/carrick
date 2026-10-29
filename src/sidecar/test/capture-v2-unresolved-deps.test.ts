/**
 * A tree is a bare checkout when a dependency its package.json declares does
 * not resolve from the service root, not only when node_modules is absent
 * (carrick#2999). A pnpm workspace whose store is missing still has each
 * package's node_modules directory, full of links to nothing.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';

const TSCONFIG = JSON.stringify({
  compilerOptions: {
    target: 'ES2020',
    module: 'commonjs',
    strict: true,
    skipLibCheck: true,
    rootDir: 'src',
  },
  include: ['src'],
});

const SERVICE_TS = 'export interface PlainResponse {\n  id: string;\n}\n';

const ANCHOR = {
  kind: 'symbol',
  alias: 'Endpoint_plain_Response',
  symbol_name: 'PlainResponse',
  source_file: 'src/service.ts',
  anchor_origin: 'llm-symbol',
} as const;

function tempDir(): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-unresolved-deps-'));
}

function writeTree(root: string, files: Record<string, string>): string {
  fs.mkdirSync(root, { recursive: true });
  for (const [name, text] of Object.entries(files)) {
    const p = path.join(root, name);
    fs.mkdirSync(path.dirname(p), { recursive: true });
    fs.writeFileSync(p, text);
  }
  return root;
}

function capture(files: Record<string, string>, links: Record<string, string> = {}) {
  const root = tempDir();
  const repoRoot = writeTree(path.join(root, 'repo'), {
    'tsconfig.json': TSCONFIG,
    'src/service.ts': SERVICE_TS,
    ...files,
  });
  for (const [name, target] of Object.entries(links)) {
    const linkPath = path.join(repoRoot, name);
    fs.mkdirSync(path.dirname(linkPath), { recursive: true });
    fs.symlinkSync(target, linkPath);
  }
  return captureStub({
    repoRoot,
    serviceName: 'unresolved-deps-svc',
    outDir: path.join(root, 'stub'),
    anchors: [ANCHOR],
  });
}

describe('capture v2: declared dependencies that do not resolve', () => {
  it('reads a pnpm tree with a missing store as bare and names the packages', () => {
    const result = capture(
      {
        'package.json': JSON.stringify({
          name: 'svc',
          dependencies: { alpha: '^1.0.0', '@scope/beta': '^2.0.0' },
          devDependencies: { gamma: '^3.0.0' },
        }),
        'node_modules/.keep': '',
      },
      {
        'node_modules/alpha': '../node_modules/.pnpm/alpha@1.0.0/node_modules/alpha',
        'node_modules/@scope/beta': '../../node_modules/.pnpm/beta@2.0.0/node_modules/@scope/beta',
        'node_modules/gamma': '../node_modules/.pnpm/gamma@3.0.0/node_modules/gamma',
      }
    );
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    assert.strictEqual(result.bare_checkout, true);
    assert.deepStrictEqual(result.unresolved_dependencies, ['@scope/beta', 'alpha', 'gamma']);
  });

  it('names at most the first five packages', () => {
    const deps: Record<string, string> = {};
    for (const n of 'abcdefgh') deps[`dep-${n}`] = '1.0.0';
    const result = capture({
      'package.json': JSON.stringify({ name: 'svc', dependencies: deps }),
      'node_modules/.keep': '',
    });
    assert.strictEqual(result.bare_checkout, true);
    assert.deepStrictEqual(result.unresolved_dependencies, [
      'dep-a',
      'dep-b',
      'dep-c',
      'dep-d',
      'dep-e',
    ]);
  });

  it('keeps a fully installed tree installed', () => {
    const result = capture({
      'package.json': JSON.stringify({
        name: 'svc',
        dependencies: { alpha: '^1.0.0' },
        devDependencies: { '@scope/beta': '^2.0.0' },
      }),
      'node_modules/alpha/package.json': JSON.stringify({ name: 'alpha', version: '1.0.0' }),
      'node_modules/@scope/beta/package.json': JSON.stringify({
        name: '@scope/beta',
        version: '2.0.0',
      }),
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    assert.strictEqual(result.bare_checkout, false);
    assert.deepStrictEqual(result.unresolved_dependencies, []);
  });

  it('resolves a hoisted dependency from a parent node_modules', () => {
    const root = tempDir();
    const repoRoot = writeTree(path.join(root, 'ws', 'svc'), {
      'tsconfig.json': TSCONFIG,
      'src/service.ts': SERVICE_TS,
      'package.json': JSON.stringify({ name: 'svc', dependencies: { alpha: '^1.0.0' } }),
      'node_modules/.keep': '',
    });
    writeTree(path.join(root, 'ws'), {
      'node_modules/alpha/package.json': JSON.stringify({ name: 'alpha', version: '1.0.0' }),
    });
    const result = captureStub({
      repoRoot,
      serviceName: 'hoisted-svc',
      outDir: path.join(root, 'stub'),
      anchors: [ANCHOR],
    });
    assert.strictEqual(result.bare_checkout, false);
    assert.deepStrictEqual(result.unresolved_dependencies, []);
  });

  it('leaves a tree with no node_modules bare with nothing named', () => {
    const result = capture({
      'package.json': JSON.stringify({ name: 'svc', dependencies: { alpha: '^1.0.0' } }),
    });
    assert.strictEqual(result.bare_checkout, true);
    assert.deepStrictEqual(result.unresolved_dependencies, []);
  });

  it('keeps an installed tree installed when one runtime dependency is missing', () => {
    const result = capture({
      'package.json': JSON.stringify({
        name: 'svc',
        dependencies: { alpha: '^1.0.0', 'optional-native': '^1.0.0' },
      }),
      'node_modules/alpha/package.json': JSON.stringify({ name: 'alpha', version: '1.0.0' }),
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    assert.strictEqual(result.bare_checkout, false);
    assert.deepStrictEqual(result.unresolved_dependencies, ['optional-native']);
  });

  it('ignores missing devDependencies when the runtime ones are present', () => {
    const result = capture({
      'package.json': JSON.stringify({
        name: 'svc',
        dependencies: { alpha: '^1.0.0' },
        devDependencies: { 'dev-a': '1.0.0', 'dev-b': '1.0.0' },
      }),
      'node_modules/alpha/package.json': JSON.stringify({ name: 'alpha', version: '1.0.0' }),
    });
    assert.strictEqual(result.bare_checkout, false);
    assert.deepStrictEqual(result.unresolved_dependencies, ['dev-a', 'dev-b']);
  });

  it('reads a tree as bare when every runtime dependency dangles', () => {
    const result = capture(
      {
        'package.json': JSON.stringify({
          name: 'svc',
          dependencies: { alpha: '^1.0.0', beta: '^2.0.0' },
          devDependencies: { gamma: '^3.0.0' },
        }),
        'node_modules/gamma/package.json': JSON.stringify({ name: 'gamma', version: '3.0.0' }),
      },
      {
        'node_modules/alpha': '../node_modules/.pnpm/alpha@1.0.0/node_modules/alpha',
        'node_modules/beta': '../node_modules/.pnpm/beta@2.0.0/node_modules/beta',
      }
    );
    assert.strictEqual(result.bare_checkout, true);
    assert.deepStrictEqual(result.unresolved_dependencies, ['alpha', 'beta']);
  });
});
