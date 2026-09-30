/**
 * carrick#1604: a service whose tsconfig lists no files of its own and only
 * `references` other projects (a "solution" config) was typed with no
 * compiler options at all.
 *
 * A solution config has no `compilerOptions`; the options live in the
 * projects it references. Both readers of the service's tsconfig parsed the
 * named file alone, so `customConditions`, `paths` and NodeNext resolution
 * never applied. A workspace import that only resolves to source through a
 * custom export condition fell through to unbuilt `dist` types, and every
 * consumer type that went through it read `unknown`.
 *
 * TypeScript's editor answers "which project types this file" by searching the
 * named config, then its references depth-first in declared order, for the
 * first project whose file list includes the file. The fixture below puts a
 * test project FIRST in the references, with different options, so "the first
 * reference" is the wrong answer and only the ownership rule passes.
 *
 * Two readers, both covered:
 *  - capture (`captureStub`): the served surface. The custom-condition import
 *    proves it; the stub's tsconfig snapshot must carry the referenced
 *    project's strictness, because the check judges under it.
 *  - the init'd project (`infer`, `bundle`, `retype_check`). The `paths`
 *    import proves it: the ts-morph program resolves it once it has the
 *    referenced options.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import type { CaptureAnchorRequest, CaptureStubResult } from '../src/capture/api.js';
import { SidecarClient } from './helpers.js';

const CLIENT_TS = `import type { Me } from "@acme/core/types";
import type { Profile } from "@shared/profile.js";

export async function me(): Promise<Me> {
  const res = await fetch("/api/v1/me");
  return (await res.json()) as Me;
}

export async function profile(): Promise<Profile> {
  const res = await fetch("/api/v1/profile");
  return (await res.json()) as Profile;
}
`;
/** 1-based lines in CLIENT_TS. */
const ME_BODY_LINE = 6;
const PROFILE_FN_LINE = 9;
const PROFILE_BODY_LINE = 11;

/** A file under the service that no referenced project includes. */
const TOOL_TS = `import type { Profile } from "@shared/profile.js";

export async function tool(): Promise<Profile> {
  const res = await fetch("/api/v1/tool");
  return (await res.json()) as Profile;
}
`;
const TOOL_BODY_LINE = 5;

/** The project the service's sources belong to. */
const SRC_CONFIG = {
  compilerOptions: {
    target: 'ES2022',
    module: 'NodeNext',
    moduleResolution: 'NodeNext',
    customConditions: ['@acme/source'],
    paths: { '@shared/*': ['./shared/*'] },
    strict: true,
    skipLibCheck: true,
  },
  include: ['src'],
};

/** A sibling project with different options, listed first on purpose. */
const TEST_CONFIG = {
  compilerOptions: { module: 'commonjs', strict: false, skipLibCheck: true },
  include: ['test'],
  references: [{ path: './tsconfig.src.json' }],
};

const tempRoots: string[] = [];

after(() => {
  for (const root of tempRoots) fs.rmSync(root, { recursive: true, force: true });
});

/**
 * A two-package workspace: `@acme/core` exports its types only through a
 * custom condition (its `dist` is never built), linked into `node_modules` the
 * way a workspace install links it. Returns the client service directory.
 * `configs` replaces the client's tsconfig files (name -> JSON).
 */
function writeWorkspace(configs: Record<string, unknown>): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1604-'));
  tempRoots.push(root);
  const files: Record<string, string> = {
    'package.json': JSON.stringify({ name: 'acme', private: true, workspaces: ['packages/*'] }),
    'packages/core/package.json': JSON.stringify({
      name: '@acme/core',
      version: '1.0.0',
      type: 'module',
      exports: {
        './types': {
          import: {
            '@acme/source': './src/types.ts',
            types: './dist/types.d.ts',
            default: './dist/types.js',
          },
        },
      },
    }),
    'packages/core/src/types.ts': 'export type Me = { id: string; email: string };\n',
    'packages/client/package.json': JSON.stringify({
      name: '@acme/client',
      version: '1.0.0',
      type: 'module',
      dependencies: { '@acme/core': 'workspace:*' },
    }),
    'packages/client/shared/profile.ts':
      'export interface Profile { handle: string; bio: string | null }\n',
    'packages/client/src/client.ts': CLIENT_TS,
    'packages/client/src/extra.ts': 'export const extra = 1;\n',
    'packages/client/test/client.test.ts':
      'import { me } from "../src/client.js";\nexport const probe = me;\n',
    'packages/client/scripts/tool.ts': TOOL_TS,
  };
  for (const [name, config] of Object.entries(configs)) {
    files[`packages/client/${name}`] = JSON.stringify(config, null, 2);
  }
  for (const [rel, text] of Object.entries(files)) {
    const abs = path.join(root, rel);
    fs.mkdirSync(path.dirname(abs), { recursive: true });
    fs.writeFileSync(abs, text);
  }
  fs.mkdirSync(path.join(root, 'node_modules', '@acme'), { recursive: true });
  fs.symlinkSync(
    path.join('..', '..', 'packages', 'core'),
    path.join(root, 'node_modules', '@acme', 'core'),
    'dir'
  );
  return path.join(root, 'packages', 'client');
}

/** The real corpus shape: a references-only root over a test and a src project. */
function solutionWorkspace(): string {
  return writeWorkspace({
    'tsconfig.json': {
      files: [],
      references: [{ path: './tsconfig.test.json' }, { path: './tsconfig.src.json' }],
    },
    'tsconfig.test.json': TEST_CONFIG,
    'tsconfig.src.json': SRC_CONFIG,
  });
}

function bodyAnchor(alias: string, file: string, line: number, text: string): CaptureAnchorRequest {
  return {
    kind: 'infer',
    alias,
    source_file: file,
    anchor_origin: 'deterministic-infer',
    line_number: line,
    expression_text: text,
  };
}

const ME_ANCHOR = bodyAnchor('Me_Body', 'src/client.ts', ME_BODY_LINE, '(await res.json()) as Me');
const PROFILE_ANCHOR = bodyAnchor(
  'Profile_Body',
  'src/client.ts',
  PROFILE_BODY_LINE,
  '(await res.json()) as Profile'
);

function capture(serviceDir: string, anchors: CaptureAnchorRequest[]): {
  result: CaptureStubResult;
  surface: string;
  snapshot: Record<string, unknown>;
} {
  const outDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1604-stub-'));
  tempRoots.push(outDir);
  const result = captureStub({
    repoRoot: serviceDir,
    serviceName: 'client',
    anchors,
    outDir,
    tsconfigPath: 'tsconfig.json',
  });
  assert.ok(result.success, `capture failed: ${result.errors.join('; ')}`);
  const surface = fs.readFileSync(path.join(outDir, 'types', 'surface.d.ts'), 'utf8');
  const snapshot = JSON.parse(
    fs.readFileSync(path.join(outDir, 'tsconfig.snapshot.json'), 'utf8')
  ) as Record<string, unknown>;
  return { result, surface, snapshot };
}

function selfCheck(result: CaptureStubResult, alias: string): string | undefined {
  return result.aliases.find((record) => record.alias === alias)?.self_check;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe('carrick#1604: capture reads the referenced project that includes the anchor', () => {
  it('types a custom-condition and a paths import under the owning project', () => {
    const { result, surface, snapshot } = capture(solutionWorkspace(), [ME_ANCHOR, PROFILE_ANCHOR]);
    assert.strictEqual(selfCheck(result, 'Me_Body'), 'ok', surface);
    assert.strictEqual(selfCheck(result, 'Profile_Body'), 'ok', surface);
    assert.match(collapse(surface), /Me_Body = \{ id: string; email: string; \}/);
    // The check judges the pair under the stub's recorded options, so the
    // snapshot must be the owning project's, not the empty solution's.
    assert.strictEqual(snapshot.strict, true);
    assert.strictEqual(snapshot.strictNullChecks, true);
    assert.strictEqual(snapshot.module, 'NodeNext');
  });

  it('terminates on a reference cycle and still finds the owner', () => {
    const service = writeWorkspace({
      'tsconfig.json': { files: [], references: [{ path: './tsconfig.a.json' }] },
      'tsconfig.a.json': { files: [], references: [{ path: './tsconfig.b.json' }] },
      'tsconfig.b.json': {
        ...SRC_CONFIG,
        // Back to its parent, and to the root that started the walk.
        references: [{ path: './tsconfig.a.json' }, { path: './tsconfig.json' }],
      },
    });
    const { result, surface } = capture(service, [ME_ANCHOR]);
    assert.strictEqual(selfCheck(result, 'Me_Body'), 'ok', surface);
  });

  it('skips a missing reference, says so, and still finds the owner', () => {
    const service = writeWorkspace({
      'tsconfig.json': {
        files: [],
        references: [{ path: './tsconfig.gone.json' }, { path: './tsconfig.src.json' }],
      },
      'tsconfig.src.json': SRC_CONFIG,
    });
    const { result, surface } = capture(service, [ME_ANCHOR]);
    assert.strictEqual(selfCheck(result, 'Me_Body'), 'ok', surface);
    assert.ok(
      result.errors.some((e) => e.includes('tsconfig.gone.json')),
      `no diagnostic names the missing reference: ${JSON.stringify(result.errors)}`
    );
  });

  it('leaves a file no referenced project includes under the named tsconfig, as before', () => {
    // No project includes scripts/, so the anchor is typed under the solution
    // config's (empty) options exactly as it was before references were read:
    // the `paths` alias does not apply and the body stays unresolved.
    const { result, surface, snapshot } = capture(solutionWorkspace(), [
      bodyAnchor('Tool_Body', 'scripts/tool.ts', TOOL_BODY_LINE, '(await res.json()) as Profile'),
    ]);
    assert.notStrictEqual(selfCheck(result, 'Tool_Body'), 'ok', surface);
    assert.strictEqual(snapshot.strict, false);
    assert.ok(
      result.errors.some((e) => e.includes('scripts/tool.ts')),
      `no diagnostic names the file no project includes: ${JSON.stringify(result.errors)}`
    );
  });
});

interface InferShape {
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    any_provenance?: unknown[];
  }>;
}

async function inferProfileReturn(serviceDir: string): Promise<{ type_string: string; any_provenance?: unknown[] }> {
  const client = new SidecarClient();
  await client.start();
  try {
    await client.send({
      action: 'init',
      request_id: 'init',
      repo_root: serviceDir,
      tsconfig_path: 'tsconfig.json',
    });
    const res = await client.send<InferShape>(
      {
        action: 'infer',
        request_id: 'profile',
        requests: [
          {
            file_path: path.join(serviceDir, 'src', 'client.ts'),
            line_number: PROFILE_FN_LINE,
            infer_kind: 'function_return',
            alias: 'Profile_Return',
          },
        ],
      },
      30000
    );
    const row = res.inferred_types?.find((t) => t.alias === 'Profile_Return');
    assert.ok(row, `no inference: ${JSON.stringify(res)}`);
    return row;
  } finally {
    await client.stop();
  }
}

describe('carrick#1604: the init\'d project reads the referenced project that includes the service', () => {
  it('resolves a paths import under the project that includes most of the service', async () => {
    const row = await inferProfileReturn(solutionWorkspace());
    assert.strictEqual(collapse(row.type_string), '{ handle: string; bio: null | string; }');
    assert.strictEqual(row.any_provenance, undefined);
  });

  it('keeps the named tsconfig when no reference can be read, as before', async () => {
    // Every reference is missing: nothing includes the service's files, so
    // the program is the solution config's, as it was before references were
    // read, and the alias import stays unresolved.
    const service = writeWorkspace({
      'tsconfig.json': { files: [], references: [{ path: './tsconfig.gone.json' }] },
    });
    const row = await inferProfileReturn(service);
    assert.strictEqual(row.type_string, 'Profile');
    assert.ok(row.any_provenance, 'an unresolved return must carry its provenance');
  });
});
