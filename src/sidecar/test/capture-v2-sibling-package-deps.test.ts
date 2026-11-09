/**
 * A sibling package's declarations that import a dependency only the sibling
 * installs still self-check clean.
 *
 * The service reads a sibling package's SOURCE, so the sibling's declarations
 * land in the stub's tree. The sibling imports a package that sits in the
 * SIBLING's node_modules and not in the service's. The self-check linked only
 * the service's node_modules, so the alias failed as an unresolved pinned
 * external and the response read unverifiable, though the checkout resolves
 * the import from the file's own location.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';

const FILES: Record<string, string> = {
  // The service installs one package, so the checkout is not bare.
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
  // The sibling installs a package the service does not.
  'packages/core/package.json': JSON.stringify({ name: 'core', private: true, dependencies: { 'core-only': '2.0.0' } }),
  'packages/core/node_modules/core-only/package.json': JSON.stringify({ name: 'core-only', version: '2.0.0', types: 'index.d.ts' }),
  'packages/core/node_modules/core-only/index.d.ts': 'export interface Span { traceId: string }\n',
  'packages/core/src/result.ts': [
    "import type { Span } from 'core-only';",
    'export interface CoreResult { zones: string[]; span: Span }',
    '',
  ].join('\n'),
  'apps/web/src/routes.ts': [
    "import type { CoreResult } from '@core/result';",
    "import type { WebDep } from 'web-dep';",
    'export interface WebResponse { result: CoreResult; dep: WebDep }',
    '',
  ].join('\n'),
};

describe('a sibling package whose own dependency the service lacks', () => {
  let base: string;
  let result: ReturnType<typeof captureStub>;

  before(() => {
    base = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-sibling-deps-'));
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
          symbol_name: 'WebResponse',
          source_file: 'src/routes.ts',
          anchor_origin: 'llm-symbol',
        },
      ],
    });
  });

  after(() => {
    fs.rmSync(base, { recursive: true, force: true });
  });

  it('self-checks the alias clean', () => {
    assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
    assert.deepStrictEqual(
      result.aliases.map((a) => [a.alias, a.self_check, a.self_check_detail]),
      [['Endpoint_web_Response', 'ok', undefined]],
      JSON.stringify(result.aliases)
    );
  });
});
