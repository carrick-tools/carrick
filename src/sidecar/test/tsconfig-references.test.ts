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
 * first project whose file list includes the file. That project owns the
 * file, and a file is only ever typed under its owner's options, on both
 * readers:
 *  - capture (`captureStub`), the served surface: each anchor is resolved in
 *    its owner's program;
 *  - the init'd project (`infer`, `bundle`, `retype_check`): each request is
 *    answered by its file's owner's program, built on first use.
 *
 * Which project has more files never decides how a file is typed: a test
 * project with more files than the source project cannot type a source file,
 * and a file two projects include is typed by the first one on both readers.
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
    // The stub records the options its surface was emitted under: the owning
    // project's, not the empty solution's.
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
    // No project includes scripts/, so the named config owns it and its anchor
    // is typed under the solution config's (empty) options, exactly as before
    // references were read: the `paths` alias does not apply and the body
    // stays unresolved. The source anchor beside it is still typed under the
    // project that owns it.
    const { result, surface } = capture(solutionWorkspace(), [
      bodyAnchor('Tool_Body', 'scripts/tool.ts', TOOL_BODY_LINE, '(await res.json()) as Profile'),
      ME_ANCHOR,
    ]);
    assert.notStrictEqual(selfCheck(result, 'Tool_Body'), 'ok', surface);
    assert.strictEqual(selfCheck(result, 'Me_Body'), 'ok', surface);
    assert.match(collapse(surface), /Me_Body = \{ id: string; email: string; \}/);
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
  it('resolves a paths import under the project that owns the file', async () => {
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

// ---------------------------------------------------------------------------
// The owning project decides, not the project with the most files.
// ---------------------------------------------------------------------------

/** Write a service tree (JSON values are serialised) under a fresh temp dir. */
function tree(files: Record<string, string | object>): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1604-owner-'));
  tempRoots.push(root);
  for (const [rel, content] of Object.entries(files)) {
    const abs = path.join(root, rel);
    fs.mkdirSync(path.dirname(abs), { recursive: true });
    fs.writeFileSync(abs, typeof content === 'string' ? content : JSON.stringify(content, null, 2));
  }
  return root;
}

/** Two functions whose return types depend on the project's options. */
const SHARED_TS = `import { thing } from "@shared/thing";
export function viaPaths() { return thing; }
export function nullable(x: string) { return Math.random() > 0.5 ? x : null; }
`;
const VIA_PATHS = { line: 2, text: 'thing' };
const NULLABLE = { line: 3, text: 'Math.random() > 0.5 ? x : null' };

const SRC_PROJECT = {
  compilerOptions: {
    module: 'NodeNext',
    moduleResolution: 'NodeNext',
    strict: true,
    skipLibCheck: true,
    paths: { '@shared/*': ['./shared-src/*'] },
  },
  include: ['src'],
};
const TEST_PROJECT = {
  compilerOptions: {
    module: 'commonjs',
    strict: false,
    skipLibCheck: true,
    paths: { '@shared/*': ['./shared-test/*'] },
  },
  include: ['test'],
};

/** A source and a test project whose `paths` and strictness disagree. */
function splitService(extra: Record<string, string | object> = {}): string {
  return tree({
    'package.json': { name: 'svc', version: '1.0.0' },
    'tsconfig.json': {
      files: [],
      references: [{ path: './tsconfig.src.json' }, { path: './tsconfig.test.json' }],
    },
    'tsconfig.src.json': SRC_PROJECT,
    'tsconfig.test.json': TEST_PROJECT,
    'shared-src/thing.ts': 'export const thing = { kind: "src" as const, n: 1 as number | null };\n',
    'shared-test/thing.ts': 'export const thing = { kind: "test" as const, label: "x" };\n',
    'src/a.ts': SHARED_TS,
    'test/t.ts': SHARED_TS,
    ...extra,
  });
}

/** Six more test files than source files: the test project is the majority. */
const TEST_MAJORITY = Object.fromEntries(
  [0, 1, 2, 3, 4, 5].map((n) => [`test/pad${n}.ts`, `export const t${n} = ${n};\n`])
);

interface Probe {
  alias: string;
  file: string;
  line: number;
  text: string;
}

/** `function_return` of every probe through the init'd project. */
async function inferReturns(serviceDir: string, probes: Probe[]): Promise<Map<string, string>> {
  const client = new SidecarClient();
  await client.start();
  try {
    await client.send({ action: 'init', request_id: 'init', repo_root: serviceDir, tsconfig_path: 'tsconfig.json' });
    const res = await client.send<InferShape>(
      {
        action: 'infer',
        request_id: 'returns',
        requests: probes.map((probe) => ({
          file_path: path.join(serviceDir, probe.file),
          line_number: probe.line,
          infer_kind: 'function_return',
          alias: probe.alias,
        })),
      },
      60000
    );
    return new Map((res.inferred_types ?? []).map((t) => [t.alias, collapse(t.type_string)]));
  } finally {
    await client.stop();
  }
}

/** The surface text and self-check of every probe's return expression. */
function captureReturns(serviceDir: string, probes: Probe[]): Map<string, { text: string; check?: string; reason?: string }> {
  const { result, surface } = capture(
    serviceDir,
    probes.map((probe) => bodyAnchor(probe.alias, probe.file, probe.line, probe.text))
  );
  return aliasesOf(result, surface);
}

function aliasesOf(
  result: CaptureStubResult,
  surface: string
): Map<string, { text: string; check?: string; reason?: string }> {
  const out = new Map<string, { text: string; check?: string; reason?: string }>();
  for (const record of result.aliases) {
    const match = collapse(surface).match(new RegExp(`type ${record.alias} = (.*?);(?: export| declare|$)`));
    out.set(record.alias, {
      text: match ? match[1] : '(none)',
      check: record.self_check,
      reason: record.capture_failure_reason,
    });
  }
  return out;
}

const probe = (alias: string, file: string, at: { line: number; text: string }): Probe => ({
  alias,
  file,
  line: at.line,
  text: at.text,
});

describe('carrick#1604: each file is typed under the project that owns it', () => {
  const PROBES = [
    probe('Src_Paths', 'src/a.ts', VIA_PATHS),
    probe('Src_Nullable', 'src/a.ts', NULLABLE),
    probe('Test_Paths', 'test/t.ts', VIA_PATHS),
    probe('Test_Nullable', 'test/t.ts', NULLABLE),
  ];

  it('never lets a test project with more files type a source file, on either reader', async () => {
    const service = splitService(TEST_MAJORITY);
    const inferred = await inferReturns(service, PROBES);
    assert.strictEqual(inferred.get('Src_Paths'), '{ kind: "src"; n: null | number; }');
    assert.strictEqual(inferred.get('Src_Nullable'), 'string | null');
    assert.strictEqual(inferred.get('Test_Paths'), '{ kind: "test"; label: string; }');
    assert.strictEqual(inferred.get('Test_Nullable'), 'string');

    const captured = captureReturns(service, PROBES);
    assert.deepStrictEqual(captured.get('Src_Paths'), { text: '{ kind: "src"; n: number | null; }', check: 'ok', reason: undefined });
    assert.deepStrictEqual(captured.get('Src_Nullable'), { text: 'string | null', check: 'ok', reason: undefined });
    assert.deepStrictEqual(captured.get('Test_Paths'), { text: '{ kind: "test"; label: string; }', check: 'ok', reason: undefined });
    assert.deepStrictEqual(captured.get('Test_Nullable'), { text: 'string', check: 'ok', reason: undefined });
  });

  it('types the minority project\'s file under its own options when the source project is the majority', () => {
    const captured = captureReturns(splitService(), [...PROBES, probe('Src_Paths_2', 'src/a.ts', VIA_PATHS)]);
    assert.strictEqual(captured.get('Test_Paths')?.text, '{ kind: "test"; label: string; }');
    assert.strictEqual(captured.get('Test_Nullable')?.text, 'string');
    assert.strictEqual(captured.get('Src_Nullable')?.text, 'string | null');
  });

  it('demotes a minority anchor that names a module the emit would declare under other options', () => {
    // The test project's symbol would be emitted under the source project's
    // `paths`, which point `@shared/thing` at another file: a wrong type that
    // self-checks clean. It is demoted instead, with the reason.
    const service = splitService({
      'test/shape.ts': 'import { thing } from "@shared/thing";\nexport type TestShape = typeof thing;\n',
    });
    const { result, surface } = capture(service, [
      bodyAnchor('Src_Paths', 'src/a.ts', VIA_PATHS.line, VIA_PATHS.text),
      bodyAnchor('Src_Nullable', 'src/a.ts', NULLABLE.line, NULLABLE.text),
      { kind: 'symbol', alias: 'Test_Shape', symbol_name: 'TestShape', source_file: 'test/shape.ts', anchor_origin: 'llm-symbol' },
    ]);
    const shape = aliasesOf(result, surface).get('Test_Shape');
    assert.strictEqual(shape?.text, 'unknown', surface);
    assert.notStrictEqual(shape?.check, 'ok');
    assert.match(shape?.reason ?? '', /tsconfig\.test\.json/);
    assert.strictEqual(aliasesOf(result, surface).get('Src_Paths')?.check, 'ok');
  });

  it('keeps that anchor when the two projects would emit it the same way', () => {
    // Same options apart from which files each project lists.
    const alike = { ...SRC_PROJECT, include: ['test'] };
    const service = splitService({
      'tsconfig.test.json': alike,
      'test/shape.ts': 'import { thing } from "@shared/thing";\nexport type TestShape = typeof thing;\n',
    });
    const { result, surface } = capture(service, [
      bodyAnchor('Src_Paths', 'src/a.ts', VIA_PATHS.line, VIA_PATHS.text),
      bodyAnchor('Src_Nullable', 'src/a.ts', NULLABLE.line, NULLABLE.text),
      { kind: 'symbol', alias: 'Test_Shape', symbol_name: 'TestShape', source_file: 'test/shape.ts', anchor_origin: 'llm-symbol' },
    ]);
    assert.strictEqual(aliasesOf(result, surface).get('Test_Shape')?.check, 'ok', surface);
  });

  it('types a file two projects include under the first one, on both readers', async () => {
    const service = tree({
      'package.json': { name: 'svc', version: '1.0.0' },
      'tsconfig.json': { files: [], references: [{ path: './tsconfig.a.json' }, { path: './tsconfig.b.json' }] },
      'tsconfig.a.json': {
        compilerOptions: { strict: true, module: 'ESNext', moduleResolution: 'Bundler', paths: { '@x/*': ['./xa/*'] } },
        include: ['src', 'shared'],
      },
      'tsconfig.b.json': {
        compilerOptions: { strict: true, module: 'ESNext', moduleResolution: 'Bundler', paths: { '@x/*': ['./xb/*'] } },
        include: ['shared', 'other'],
      },
      'xa/thing.ts': 'export const thing = { from: "a" as const };\n',
      'xb/thing.ts': 'export const thing = { from: "b" as const };\n',
      'shared/s.ts': 'import { thing } from "@x/thing";\nexport function viaPaths() { return thing; }\n',
      'src/a.ts': 'export const a = 1;\n',
      'other/o1.ts': 'export const o1 = 1;\n',
      'other/o2.ts': 'export const o2 = 1;\n',
      'other/o3.ts': 'export const o3 = 1;\n',
    });
    const probes = [probe('Shared', 'shared/s.ts', VIA_PATHS)];
    assert.strictEqual((await inferReturns(service, probes)).get('Shared'), '{ from: "a"; }');
    assert.deepStrictEqual(captureReturns(service, probes).get('Shared'), { text: '{ from: "a"; }', check: 'ok', reason: undefined });
  });

  it('types a referenced project\'s file under that project when the named tsconfig has files of its own', async () => {
    const service = tree({
      'package.json': { name: 'svc', version: '1.0.0' },
      'tsconfig.json': {
        compilerOptions: { module: 'commonjs', strict: false, paths: { '@shared/*': ['./shared-root/*'] } },
        include: ['scripts'],
        references: [{ path: './tsconfig.src.json' }],
      },
      'tsconfig.src.json': SRC_PROJECT,
      'scripts/s.ts': 'export const s = 1;\n',
      'shared-root/thing.ts': 'export const thing = { kind: "root" as const };\n',
      'shared-src/thing.ts': 'export const thing = { kind: "src" as const, n: 1 as number | null };\n',
      'src/a.ts': SHARED_TS,
    });
    const probes = [probe('Src_Paths', 'src/a.ts', VIA_PATHS)];
    assert.strictEqual((await inferReturns(service, probes)).get('Src_Paths'), '{ kind: "src"; n: null | number; }');
    assert.strictEqual(captureReturns(service, probes).get('Src_Paths')?.text, '{ kind: "src"; n: number | null; }');
  });

  it('resolves a dual package per file: Bundler imports, Node16 CommonJS requires', async () => {
    const DUAL_TS = 'import { v } from "dual";\nexport function dualV() { return { ...v }; }\n';
    const service = tree({
      'package.json': { name: 'svc', version: '1.0.0' },
      'tsconfig.json': { files: [], references: [{ path: './tsconfig.app.json' }, { path: './tsconfig.node.json' }] },
      'tsconfig.app.json': {
        compilerOptions: { module: 'ESNext', moduleResolution: 'Bundler', strict: true, skipLibCheck: true, target: 'ES2022' },
        include: ['src'],
      },
      'tsconfig.node.json': {
        compilerOptions: { module: 'Node16', moduleResolution: 'Node16', strict: true, skipLibCheck: true, target: 'ES2022' },
        include: ['server'],
      },
      'node_modules/dual/package.json': {
        name: 'dual',
        version: '1.0.0',
        exports: { '.': { import: { types: './esm.d.mts' }, require: { types: './cjs.d.cts' } } },
      },
      'node_modules/dual/esm.d.mts': 'export interface Shape { id: string; esm: true }\nexport declare const v: Shape;\n',
      'node_modules/dual/cjs.d.cts': 'export interface Shape { id: number; cjs: true }\nexport declare const v: Shape;\n',
      'src/app.ts': DUAL_TS,
      ...Object.fromEntries([0, 1, 2, 3].map((n) => [`src/p${n}.ts`, `export const p${n} = ${n};\n`])),
      'server/srv.ts': DUAL_TS,
    });
    const at = { line: 2, text: '{ ...v }' };
    const probes = [probe('App_Dual', 'src/app.ts', at), probe('Server_Dual', 'server/srv.ts', at)];
    const inferred = await inferReturns(service, probes);
    assert.strictEqual(inferred.get('App_Dual'), '{ id: string; esm: true; }');
    assert.strictEqual(inferred.get('Server_Dual'), '{ id: number; cjs: true; }');
    const captured = captureReturns(service, probes);
    assert.strictEqual(captured.get('App_Dual')?.text, '{ id: string; esm: true; }');
    assert.strictEqual(captured.get('Server_Dual')?.text, '{ id: number; cjs: true; }');
  });
});
