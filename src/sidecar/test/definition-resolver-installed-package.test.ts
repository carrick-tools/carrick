/**
 * carrick#3995 T5: the definition resolver resolves a bare package specifier
 * the way the capture's self-check did.
 *
 * The capture writes its stub outside the checkout and self-checks it with the
 * producer's installed packages in reach. The resolver runs on the same stub
 * afterwards. When it read the stub with nothing installed, an alias the
 * self-check called clean, `import("pkg").Thing`, answered `any`, and the
 * entry published no definition. Both now read the stub through one lookup.
 *
 * Each case runs the real capture and then the resolver, in that order, as
 * the compiler engine's request loop does.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import type { CaptureStubResult } from '../src/capture/api.js';
import { DefinitionResolver } from '../src/definition-resolver.js';
import { UNTRUSTED_TREE_ENV } from '../src/capture/checkout.js';

const SOURCE_TS = `import { makeThing } from 'pkg';

export const thing = makeThing();
`;

const PKG_DTS = `export declare class Thing {
  name: string;
  send(event: string): void;
}
export declare function makeThing(): Thing;
`;

const ALIAS = 'Capture_thing';

/** Capture `thing` in a service whose `pkg` is installed or not. */
function capture(installed: boolean): { repoDir: string; outRoot: string; result: CaptureStubResult } {
  const repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-3995-t5-'));
  const outRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-3995-t5-stub-'));
  fs.mkdirSync(path.join(repoDir, 'src'));
  fs.writeFileSync(
    path.join(repoDir, 'tsconfig.json'),
    JSON.stringify({
      compilerOptions: {
        strict: true,
        module: 'esnext',
        moduleResolution: 'bundler',
        target: 'es2022',
        skipLibCheck: true,
      },
      include: ['src'],
    })
  );
  fs.writeFileSync(
    path.join(repoDir, 'package.json'),
    JSON.stringify({ name: 'svc', version: '1.0.0', dependencies: { pkg: '1.0.0' } })
  );
  fs.writeFileSync(path.join(repoDir, 'src', 'thing.ts'), SOURCE_TS);
  if (installed) {
    const pkgDir = path.join(repoDir, 'node_modules', 'pkg');
    fs.mkdirSync(pkgDir, { recursive: true });
    fs.writeFileSync(
      path.join(pkgDir, 'package.json'),
      JSON.stringify({ name: 'pkg', version: '1.0.0', types: 'index.d.ts' })
    );
    fs.writeFileSync(path.join(pkgDir, 'index.d.ts'), PKG_DTS);
  }
  const result = captureStub({
    repoRoot: repoDir,
    serviceName: 'svc',
    outDir: path.join(outRoot, 'stub'),
    anchors: [
      {
        kind: 'infer' as const,
        alias: ALIAS,
        source_file: 'src/thing.ts',
        anchor_origin: 'deterministic-infer' as const,
        expression_text: 'makeThing()',
        line_number: 3,
      },
    ],
  });
  assert.strictEqual(result.success, true, JSON.stringify(result.errors));
  return { repoDir, outRoot, result };
}

describe('carrick#3995 T5: the resolver reads a bare package import as the self-check did', () => {
  const made: string[] = [];
  after(() => {
    for (const dir of made) fs.rmSync(dir, { recursive: true, force: true });
  });

  describe('the package is installed beside the service', () => {
    let result: CaptureStubResult;
    before(() => {
      const captured = capture(true);
      made.push(captured.repoDir, captured.outRoot);
      result = captured.result;
    });

    it('the stub names the type through the package (premise)', () => {
      const surface = fs.readFileSync(path.join(result.stub_dir, 'types', 'surface.d.ts'), 'utf8');
      assert.match(surface, new RegExp(`export type ${ALIAS} = import\\("pkg"\\)\\.Thing;`), surface);
      const record = result.aliases.find((a) => a.alias === ALIAS);
      assert.ok(record, 'no capture record');
      assert.ok(
        !(record!.unresolved_in_tree ?? []).some((p) => p.reason === 'unresolved_import'),
        `the self-check called the alias clean: ${JSON.stringify(record)}`
      );
    });

    // A type a package declares stays by name in the structural form, as the
    // inference path prints it; what changes is that it is the type, not any.
    it('resolves to the package type, not any', () => {
      const [definition] = new DefinitionResolver().resolveFromStub(result.stub_dir, [ALIAS]);
      assert.ok(definition, 'no definition');
      assert.strictEqual(definition.expanded, 'Thing', JSON.stringify(definition));
      assert.match(definition.definition, /import\("pkg"\)\.Thing/, JSON.stringify(definition));
    });

    it('leaves no link to the installed packages in the stub', () => {
      new DefinitionResolver().resolveFromStub(result.stub_dir, [ALIAS]);
      assert.strictEqual(fs.existsSync(path.join(result.stub_dir, 'node_modules')), false);
    });
  });

  describe('the package is gone by the time the resolver reads the stub', () => {
    let result: CaptureStubResult;
    before(() => {
      const captured = capture(true);
      made.push(captured.repoDir, captured.outRoot);
      result = captured.result;
      fs.rmSync(path.join(captured.repoDir, 'node_modules', 'pkg'), { recursive: true });
    });

    it('still answers any: the shape comes from the installed package, nowhere else', () => {
      const [definition] = new DefinitionResolver().resolveFromStub(result.stub_dir, [ALIAS]);
      assert.ok(definition, 'no definition');
      assert.strictEqual(definition.expanded, 'any', JSON.stringify(definition));
    });
  });

  // The self-check's host resolves a bare specifier in a declaration emitted
  // for a source outside the service's rootDir from that source's place in the
  // checkout (carrick#3633). The resolver reads it the same way.
  describe("a sibling package's own dependency, reached from outside the service root", () => {
    const FILES: Record<string, string> = {
      'apps/web/package.json': JSON.stringify({ name: 'web', private: true, dependencies: { 'web-dep': '1.0.0' } }),
      'apps/web/node_modules/web-dep/package.json': JSON.stringify({ name: 'web-dep', version: '1.0.0', types: 'index.d.ts' }),
      'apps/web/node_modules/web-dep/index.d.ts': 'export interface WebDep { ok: boolean }\n',
      'apps/web/tsconfig.json': JSON.stringify({
        compilerOptions: {
          target: 'ES2022',
          module: 'ESNext',
          moduleResolution: 'Bundler',
          strict: true,
          skipLibCheck: true,
          paths: { '@core/*': ['../../packages/core/src/*'] },
        },
        include: ['src'],
      }),
      'packages/core/package.json': JSON.stringify({ name: 'core', private: true, dependencies: { 'core-only': '2.0.0' } }),
      'packages/core/node_modules/core-only/package.json': JSON.stringify({ name: 'core-only', version: '2.0.0', types: 'index.d.ts' }),
      'packages/core/node_modules/core-only/index.d.ts': 'export interface Span { traceId: string }\n',
      'packages/core/src/result.ts': "import type { Span } from 'core-only';\nexport type CoreSpan = Span;\n",
      'apps/web/src/routes.ts': "import type { CoreSpan } from '@core/result';\nexport type WebSpan = CoreSpan;\n",
    };
    let result: CaptureStubResult;
    before(() => {
      const base = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-3995-t5-sibling-'));
      made.push(base);
      const repo = path.join(base, 'repo');
      for (const [rel, text] of Object.entries(FILES)) {
        fs.mkdirSync(path.dirname(path.join(repo, rel)), { recursive: true });
        fs.writeFileSync(path.join(repo, rel), text);
      }
      result = captureStub({
        repoRoot: path.join(repo, 'apps/web'),
        serviceName: 'web',
        outDir: path.join(base, 'stub'),
        anchors: [
          {
            kind: 'symbol',
            alias: 'Endpoint_web_Response',
            symbol_name: 'WebSpan',
            source_file: 'src/routes.ts',
            anchor_origin: 'llm-symbol',
          },
        ],
      });
      assert.strictEqual(result.success, true, JSON.stringify(result.errors));
      assert.deepStrictEqual(
        result.aliases.map((a) => [a.alias, a.self_check]),
        [['Endpoint_web_Response', 'ok']],
        JSON.stringify(result.aliases)
      );
    });

    it("resolves to the sibling's package type, not any", () => {
      const [definition] = new DefinitionResolver().resolveFromStub(result.stub_dir, ['Endpoint_web_Response']);
      assert.ok(definition, 'no definition');
      assert.strictEqual(definition.expanded, 'Span', JSON.stringify(definition));
    });
  });

  // Under CARRICK_UNTRUSTED_TREE the self-check's host is the bounded one; the
  // resolver resolves through it and still reads a package the checkout holds.
  describe('an untrusted tree', () => {
    let saved: string | undefined;
    before(() => {
      saved = process.env[UNTRUSTED_TREE_ENV];
      process.env[UNTRUSTED_TREE_ENV] = '1';
    });
    after(() => {
      if (saved === undefined) delete process.env[UNTRUSTED_TREE_ENV];
      else process.env[UNTRUSTED_TREE_ENV] = saved;
    });

    it('resolves a package installed inside the checkout', () => {
      const captured = capture(true);
      made.push(captured.repoDir, captured.outRoot);
      const [definition] = new DefinitionResolver().resolveFromStub(captured.result.stub_dir, [ALIAS]);
      assert.strictEqual(definition?.expanded, 'Thing', JSON.stringify(definition));
    });

  });
});
