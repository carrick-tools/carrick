/**
 * The capture's declaration emit and the self-check's diagnostics run on
 * TypeScript 7's native tsc beside ts-morph (carrick#3133).
 *
 * Neither step asks the checker a question: the emit writes declarations
 * without checking, and the self-check reads only which modules and names the
 * stub cannot resolve. Both moved to the native compiler, as the judge did
 * (carrick#2951); the deep walk and the anchors stay on TypeScript 6. The
 * records must not move: the same aliases, the same self-check outcome, the
 * same dangling specifiers and unfound names, whichever compiler answered.
 * Where the native compiler is not installed, both steps run on TypeScript 6
 * as before.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import ts from 'typescript';
import { captureStub } from '../src/capture/index.js';
import type { CaptureStubOptions, CaptureStubResult } from '../src/capture/api.js';
import { nativeMemoryLimit, nativeTscPath } from '../src/capture/check.js';
import { runNativeTsc, tsconfigCompilerOptions, typescript6Steps } from '../src/capture/native-tsc.js';
import { SELF_CHECK_OPTIONS } from '../src/capture/self-check.js';

const NATIVE = nativeTscPath();

/** A project every option of which TypeScript 7 accepts. */
const TSCONFIG = JSON.stringify({
  compilerOptions: { target: 'ES2020', module: 'esnext', moduleResolution: 'bundler', esModuleInterop: true, strict: true, declaration: true, rootDir: './' },
  include: ['**/*.ts'],
});

/** A CommonJS project that names no resolution: TypeScript 5 read it under
 * `node10`, which TypeScript 7 removed. */
const COMMONJS_TSCONFIG = JSON.stringify({
  compilerOptions: { target: 'ES2020', module: 'commonjs', strict: true, declaration: true, esModuleInterop: true, rootDir: './' },
  include: ['**/*.ts'],
});

/** Write the fixture repo: one clean model, and one whose declaration names a
 * module that does not exist (TS2307) and a name nothing declares (TS2304),
 * both in the closure of one alias. */
function writeRepo(repoRoot: string, tsconfig: string): void {
  fs.mkdirSync(path.join(repoRoot, 'src'), { recursive: true });
  fs.writeFileSync(path.join(repoRoot, 'tsconfig.json'), tsconfig);
  fs.writeFileSync(path.join(repoRoot, 'src', 'clean.ts'), "export interface Row { id: string; label: string }\nexport const kind = 'row';\n");
  fs.writeFileSync(
    path.join(repoRoot, 'src', 'broken.ts'),
    "import type { Missing } from './nowhere';\nexport interface Broken { id: string; gone: Missing; extra: Unfound }\n"
  );
}

function anchorsOf(): CaptureStubOptions['anchors'] {
  return [
    { kind: 'symbol', alias: 'Endpoint_clean_Response', symbol_name: 'Row', source_file: 'src/clean.ts', anchor_origin: 'llm-symbol' },
    { kind: 'symbol', alias: 'Endpoint_broken_Response', symbol_name: 'Broken', source_file: 'src/broken.ts', anchor_origin: 'llm-symbol' },
  ];
}

interface Counted {
  result: CaptureStubResult;
  /** TypeScript 6 diagnostics calls during the capture. */
  diagnosticsCalls: number;
  /** TypeScript 6 declaration emits during the capture. */
  emitCalls: number;
  /** The stub's declaration files, tree-relative, by text. */
  stubTexts: Map<string, string>;
}

/** Run a capture with TypeScript 6's diagnostics and emit counted. */
function countedCapture(opts: CaptureStubOptions & { nativeTscPath: string | null }): Counted {
  const { preEmitDiagnostics, emit } = typescript6Steps;
  let diagnosticsCalls = 0;
  let emitCalls = 0;
  typescript6Steps.preEmitDiagnostics = (program) => {
    diagnosticsCalls++;
    return preEmitDiagnostics(program);
  };
  typescript6Steps.emit = (program, writeFile) => {
    emitCalls++;
    return emit(program, writeFile);
  };
  let result: CaptureStubResult;
  try {
    result = captureStub(opts);
  } finally {
    typescript6Steps.preEmitDiagnostics = preEmitDiagnostics;
    typescript6Steps.emit = emit;
  }
  const stubTexts = new Map<string, string>();
  const typesDir = path.join(opts.outDir, 'types');
  const walk = (dir: string) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const p = path.join(dir, entry.name);
      if (entry.isDirectory()) walk(p);
      else stubTexts.set(path.relative(typesDir, p), fs.readFileSync(p, 'utf8'));
    }
  };
  if (fs.existsSync(typesDir)) walk(typesDir);
  return { result, diagnosticsCalls, emitCalls, stubTexts };
}

describe('capture v2: emit and self-check diagnostics on TypeScript 7 native tsc (carrick#3133)', { skip: NATIVE === undefined && 'the native compiler is not installed on this host' }, () => {
  let outRoot: string;
  let native: Counted;
  let ts6: Counted;

  before(() => {
    outRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-capture-3133-'));
    const repoRoot = path.join(outRoot, 'repo');
    writeRepo(repoRoot, TSCONFIG);
    const base = { repoRoot, serviceName: 'native-svc', anchors: anchorsOf() };
    native = countedCapture({ ...base, outDir: path.join(outRoot, 'stub-native'), nativeTscPath: NATIVE! });
    ts6 = countedCapture({ ...base, outDir: path.join(outRoot, 'stub-ts6'), nativeTscPath: null });
  });

  after(() => {
    fs.rmSync(outRoot, { recursive: true, force: true });
  });

  it('reads the self-check diagnostics from the native compiler, never from TypeScript 6', () => {
    assert.strictEqual(native.result.success, true, native.result.errors.join('; '));
    assert.strictEqual(native.diagnosticsCalls, 0);
  });

  it('emits the declarations on the native compiler, never on TypeScript 6', () => {
    assert.strictEqual(native.emitCalls, 0);
  });

  it('gives every alias the record TypeScript 6 gives it', () => {
    assert.deepStrictEqual(native.result.aliases, ts6.result.aliases);
    const broken = native.result.aliases.find((r) => r.alias === 'Endpoint_broken_Response')!;
    // The missing module is repaired to `unknown` (carrick#1397); the name
    // nothing declares is named beside the position it leaves untyped.
    assert.notStrictEqual(broken.self_check, 'ok');
    assert.match(JSON.stringify(broken), /Unfound/);
    const clean = native.result.aliases.find((r) => r.alias === 'Endpoint_clean_Response')!;
    assert.strictEqual(clean.self_check, 'ok');
  });

  it('writes the same stub files, their texts differing at most in quote style', () => {
    assert.deepStrictEqual([...native.stubTexts.keys()].sort(), [...ts6.stubTexts.keys()].sort());
    const unquoted = (text: string) => text.replace(/['"]/g, '"');
    for (const [file, text] of native.stubTexts) {
      assert.strictEqual(unquoted(text), unquoted(ts6.stubTexts.get(file)!), file);
    }
  });

  it('runs both steps on TypeScript 6 where the native compiler is not installed', () => {
    assert.strictEqual(ts6.result.success, true, ts6.result.errors.join('; '));
    assert.ok(ts6.diagnosticsCalls > 0);
    assert.ok(ts6.emitCalls > 0);
  });
});

describe('capture v2: an option the native compiler refuses keeps the emit on TypeScript 6 (carrick#3133)', { skip: NATIVE === undefined && 'the native compiler is not installed on this host' }, () => {
  let outRoot: string;
  let native: Counted;
  let ts6: Counted;

  before(() => {
    outRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-capture-3133-cjs-'));
    const repoRoot = path.join(outRoot, 'repo');
    writeRepo(repoRoot, COMMONJS_TSCONFIG);
    const base = { repoRoot, serviceName: 'native-cjs-svc', anchors: anchorsOf() };
    native = countedCapture({ ...base, outDir: path.join(outRoot, 'stub-native'), nativeTscPath: NATIVE! });
    ts6 = countedCapture({ ...base, outDir: path.join(outRoot, 'stub-ts6'), nativeTscPath: null });
  });

  after(() => {
    fs.rmSync(outRoot, { recursive: true, force: true });
  });

  it('emits on TypeScript 6 and still reads the self-check from the native compiler', () => {
    assert.strictEqual(native.result.success, true, native.result.errors.join('; '));
    assert.ok(native.emitCalls > 0);
    assert.strictEqual(native.diagnosticsCalls, 0);
  });

  it('gives every alias the record TypeScript 6 gives it, and the same stub', () => {
    assert.deepStrictEqual(native.result.aliases, ts6.result.aliases);
    assert.deepStrictEqual(native.stubTexts, ts6.stubTexts);
  });
});

describe('native tsc lookup (carrick#3133)', () => {
  it('finds no native compiler where its platform package is not installed', () => {
    const empty = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-no-native-'));
    try {
      assert.strictEqual(nativeTscPath(empty, 'linux', 'nonesuch'), undefined);
    } finally {
      fs.rmSync(empty, { recursive: true, force: true });
    }
  });
});

describe('native tsc run guards (carrick#3133)', { skip: NATIVE === undefined && 'the native compiler is not installed on this host' }, () => {
  let dir: string;
  before(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-native-guard-'));
    fs.writeFileSync(path.join(dir, 'a.ts'), "import { x } from './nope';\nexport type A = Missing;\n");
  });
  after(() => fs.rmSync(dir, { recursive: true, force: true }));

  const config = (compilerOptions: Record<string, unknown>) => {
    const file = path.join(dir, 'tsconfig.json');
    fs.writeFileSync(file, JSON.stringify({ compilerOptions, files: [path.join(dir, 'a.ts')] }));
    return file;
  };

  it('reads the diagnostics and the program files', () => {
    const run = runNativeTsc({ tscPath: NATIVE!, bound: undefined }, config({ noEmit: true, types: [] }), dir, ['--listFiles']);
    assert.ok(run.ok, !run.ok ? run.reason : '');
    assert.deepStrictEqual(run.diagnostics.map((d) => d.code).sort(), [2304, 2307]);
    assert.ok(run.files.some((f) => f.endsWith(`${path.sep}a.ts`)));
  });

  it('refuses options the native compiler rejects rather than reading no diagnostics', () => {
    const run = runNativeTsc({ tscPath: NATIVE!, bound: undefined }, config({ noEmit: true, target: 'es5' }), dir);
    assert.strictEqual(run.ok, false);
    assert.match(!run.ok ? run.reason : '', /refused the options: TS5108/);
  });

  it('refuses a program that read a file outside the read bound', () => {
    const bound = { roots: [], allows: (file: string) => !file.endsWith('a.ts') };
    const run = runNativeTsc({ tscPath: NATIVE!, bound }, config({ noEmit: true, types: [] }), dir, ['--listFiles']);
    assert.strictEqual(run.ok, false);
    assert.match(!run.ok ? run.reason : '', /outside the read bound/);
  });

  it('never reads an out-of-memory end as a clean program', () => {
    const fake = path.join(dir, 'fake-tsc');
    fs.writeFileSync(fake, '#!/bin/sh\necho "fatal error: runtime: out of memory"\nexit 2\n', { mode: 0o755 });
    const run = runNativeTsc({ tscPath: fake, bound: undefined }, config({ noEmit: true }), dir);
    assert.strictEqual(run.ok, false);
  });

  it('never reads a diagnostic with no file as a clean program', () => {
    const fake = path.join(dir, 'fake-tsc-global');
    fs.writeFileSync(fake, '#!/bin/sh\necho "error TS6053: File not found."\nexit 1\n', { mode: 0o755 });
    const run = runNativeTsc({ tscPath: fake, bound: undefined }, config({ noEmit: true }), dir);
    assert.strictEqual(run.ok, false);
  });
});

describe('options as a tsconfig (carrick#3133)', () => {
  it('writes the self-check options so that the compiler reads them back unchanged', () => {
    const { compilerOptions, unconverted } = tsconfigCompilerOptions(SELF_CHECK_OPTIONS);
    assert.deepStrictEqual(unconverted, []);
    const parsed = ts.parseJsonConfigFileContent({ compilerOptions, files: [] }, ts.sys, '/');
    const { configFilePath: _c, ...readBack } = parsed.options;
    assert.deepStrictEqual(readBack, { ...SELF_CHECK_OPTIONS });
  });

  it('states every self-check option in a form TypeScript 7 accepts', () => {
    assert.strictEqual(SELF_CHECK_OPTIONS.target, ts.ScriptTarget.ES2015);
    assert.strictEqual(SELF_CHECK_OPTIONS.esModuleInterop, true);
    assert.strictEqual(SELF_CHECK_OPTIONS.alwaysStrict, true);
  });

  it('makes paths absolute against the directory they were written in', () => {
    const { compilerOptions } = tsconfigCompilerOptions({
      paths: { '@/*': ['./src/*'] },
      pathsBasePath: '/repo/app',
    } as ts.CompilerOptions);
    assert.deepStrictEqual(compilerOptions.paths, { '@/*': [path.resolve('/repo/app', './src/*')] });
  });
});

describe('native compiler memory limit (carrick#3133)', () => {
  it('gives the compiler what is left of the sidecar heap cap', () => {
    assert.strictEqual(nativeMemoryLimit(['--max-old-space-size=8192'], 3000), '5192MiB');
  });
  it('never goes below the floor', () => {
    assert.strictEqual(nativeMemoryLimit(['--max-old-space-size=4096'], 4000), '512MiB');
  });
  it('sets none when the sidecar has no cap', () => {
    assert.strictEqual(nativeMemoryLimit([], 3000), undefined);
  });
});
