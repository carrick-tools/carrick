/**
 * v2 check core — abnormal tsc termination (adversarial-review finding 3).
 *
 * runCheck used to ignore the tsc exit code entirely: an OOM/SIGKILL
 * (code null), an internal crash, or a fileless config error (TS18003
 * prints WITHOUT a `(line,col)` prefix, so parseTscOutput yields zero
 * diagnostics) all fell through classifyPair's empty-diagnostics branch and
 * every pair read COMPATIBLE with success:true — the dangerous direction.
 *
 * These tests inject a stand-in tsc via CheckOptions.tscPath (the same
 * injection seam as pnpmPath) to pin:
 *  1. non-zero exit + zero parseable diagnostics -> every pair unverifiable,
 *     success:false, degraded services recorded;
 *  2. signal death (exit code null) -> same;
 *  3. non-zero exit WITH parseable probe diagnostics stays the legitimate
 *     incompatible path (tsc exits 1 on ordinary type errors — the guard
 *     must NOT be "fail on non-zero").
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { runCheck } from '../src/capture/index.js';
import {
  carriesHeapLimitLine,
  childHeap,
  judgeTscPath,
  resolveVendored,
} from '../src/capture/check.js';
import { buildProbe, decisiveAssignmentLine } from '../src/capture/check-probe.js';
import type { CheckPairSpec, CheckStubInput } from '../src/capture/api.js';

let root: string;
let stubs: CheckStubInput[];

function writeStub(dir: string, serviceName: string, surface: string): void {
  const stubDir = path.join(dir, serviceName);
  fs.mkdirSync(path.join(stubDir, 'types'), { recursive: true });
  fs.writeFileSync(
    path.join(stubDir, 'package.json'),
    JSON.stringify(
      {
        name: `@carrick/${serviceName}`,
        version: '0.0.0-carrick',
        private: true,
        types: './types/surface.d.ts',
      },
      null,
      2
    ) + '\n'
  );
  fs.writeFileSync(path.join(stubDir, 'types', 'surface.d.ts'), surface);
}

function fakeTsc(name: string, script: string): string {
  const file = path.join(root, name);
  fs.writeFileSync(file, `#!/bin/sh\n${script}\n`);
  fs.chmodSync(file, 0o755);
  return file;
}

const PAIRS: CheckPairSpec[] = [
  {
    pair_key: 'pair-a',
    protocol: 'http',
    type_kind: 'response',
    producer: { service_name: 'orders', alias: 'P_Res' },
    consumer: { service_name: 'web', alias: 'C_Res' },
  },
  {
    pair_key: 'pair-b',
    protocol: 'http',
    type_kind: 'request',
    producer: { service_name: 'orders', alias: 'P_Req' },
    consumer: { service_name: 'web', alias: 'C_Req' },
  },
];

describe('check_v2: abnormal tsc termination is never read as compatible', () => {
  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-check-v2-tsc-'));
    writeStub(
      root,
      'orders',
      'export type P_Res = { a: string; };\nexport type P_Req = { a: string; };\n'
    );
    writeStub(
      root,
      'web',
      'export type C_Res = { a: string; };\nexport type C_Req = { a: string; };\n'
    );
    stubs = [
      { service_name: 'orders', stub_dir: path.join(root, 'orders') },
      { service_name: 'web', stub_dir: path.join(root, 'web') },
    ];
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('crash exit with no parseable diagnostics -> all pairs unverifiable', async () => {
    const tscPath = fakeTsc(
      'tsc-crash',
      "echo 'error TS18003: No inputs were found in config file.' >&2; exit 1"
    );
    const result = await runCheck({ stubs, pairs: PAIRS, tscPath });
    assert.strictEqual(result.success, false);
    assert.strictEqual(result.install_ok, true);
    assert.strictEqual(result.verdicts.length, PAIRS.length);
    for (const v of result.verdicts) {
      assert.strictEqual(v.bucket, 'unverifiable', JSON.stringify(v));
      assert.strictEqual(v.gate, 'tsc:abnormal-termination');
      assert.match(v.diagnostic ?? '', /terminated abnormally/);
      // carrick#1833: the scanner reads each pair's own verdict on a failed
      // check, so the compiler's output rides the verdict, not only `errors`.
      assert.match(v.diagnostic ?? '', /exit code 1/);
      assert.match(v.diagnostic ?? '', /TS18003/);
      assert.strictEqual(v.unresolved_reason, v.diagnostic);
    }
    assert.strictEqual(result.degraded_services.length, 2);
    assert.match(result.errors.join('\n'), /exit code 1/);
    assert.match(result.errors.join('\n'), /TS18003/);
  });

  it('signal death (exit code null, e.g. OOM SIGKILL) -> all pairs unverifiable', async () => {
    const tscPath = fakeTsc('tsc-sigkill', 'kill -KILL $$');
    const result = await runCheck({ stubs, pairs: PAIRS, tscPath });
    assert.strictEqual(result.success, false);
    for (const v of result.verdicts) {
      assert.strictEqual(v.bucket, 'unverifiable', JSON.stringify(v));
      assert.strictEqual(v.gate, 'tsc:abnormal-termination');
      assert.match(v.diagnostic ?? '', /exit code null/);
    }
    assert.match(result.errors.join('\n'), /exit code null/);
  });

  it('a compiler that aborts on a full heap says so on the answer (carrick#2209)', async () => {
    const tscPath = fakeTsc(
      'tsc-heap',
      "echo 'FATAL ERROR: Reached heap limit Allocation failed - JavaScript heap out of memory' >&2; kill -ABRT $$"
    );
    const result = await runCheck({ stubs, pairs: PAIRS, tscPath });
    assert.strictEqual(result.success, false);
    assert.deepStrictEqual(result.compiler_end, {
      exit_code: null,
      signal: 'SIGABRT',
      heap_limit: true,
      heap_cap_mb: null,
    });
    for (const v of result.verdicts) {
      assert.strictEqual(v.bucket, 'unverifiable', JSON.stringify(v));
      assert.strictEqual(v.gate, 'tsc:abnormal-termination');
      assert.match(v.diagnostic ?? '', /exit code null/);
    }
  });

  it('a killed compiler is not a heap death, and a crash names its exit code', async () => {
    const killed = await runCheck({ stubs, pairs: PAIRS, tscPath: fakeTsc('tsc-kill', 'kill -KILL $$') });
    assert.deepStrictEqual(killed.compiler_end, {
      exit_code: null,
      signal: 'SIGKILL',
      heap_limit: false,
      heap_cap_mb: null,
    });
    const crashed = await runCheck({
      stubs,
      pairs: PAIRS,
      tscPath: fakeTsc('tsc-crash-2', "echo 'error TS18003: No inputs.' >&2; exit 2"),
    });
    assert.deepStrictEqual(crashed.compiler_end, {
      exit_code: 2,
      signal: null,
      heap_limit: false,
      heap_cap_mb: null,
    });
  });

  it('a compiler a signal ended after printing a diagnostic clears no pair (carrick#2209)', async () => {
    const plan = buildProbe(PAIRS[0], (s) => `@carrick/${s}`);
    const probeFile = `packages/carrick-probes/probes/${plan.fileName}`;
    const tscPath = fakeTsc(
      'tsc-partial-abort',
      `echo "${probeFile}(${decisiveAssignmentLine(plan)},7): error TS2322: Type 'A' is not assignable to type 'B'."; kill -ABRT $$`
    );
    const result = await runCheck({ stubs, pairs: PAIRS, tscPath });
    assert.strictEqual(result.success, false);
    assert.strictEqual(result.compiler_end?.signal, 'SIGABRT');
    for (const v of result.verdicts) {
      assert.strictEqual(v.bucket, 'unverifiable', JSON.stringify(v));
      assert.strictEqual(v.gate, 'tsc:abnormal-termination');
    }
  });

  it('the compiler gets what is left of the sidecar cap, and says so (carrick#2209)', () => {
    // The sidecar gets its cap as a node flag, which a child does not inherit;
    // the compiler must see its share through NODE_OPTIONS. A fresh node with
    // a cap far above what it holds runs the check against a compiler that
    // prints its NODE_OPTIONS; a small cap leaves the compiler at the default.
    const tscPath = fakeTsc('tsc-env', 'echo "options: [$NODE_OPTIONS]" >&2; exit 1');
    const checkModule = pathToFileURL(
      path.join(path.dirname(fileURLToPath(import.meta.url)), '..', 'src', 'capture', 'index.js')
    ).href;
    const script =
      `const { runCheck } = await import(${JSON.stringify(checkModule)});` +
      `const r = await runCheck(${JSON.stringify({ stubs, pairs: PAIRS, tscPath })});` +
      'process.stdout.write(JSON.stringify({ errors: r.errors.join("\\n"), cap: r.compiler_end.heap_cap_mb }));';
    const run = (capMb: number) => {
      const out = spawnSync(
        process.execPath,
        [`--max-old-space-size=${capMb}`, '--input-type=module', '-e', script],
        { encoding: 'utf8', env: { ...process.env, NODE_OPTIONS: '' } }
      );
      assert.strictEqual(out.status, 0, out.stderr);
      return JSON.parse(out.stdout) as { errors: string; cap: number | null };
    };
    const large = run(20000);
    assert.ok(large.cap !== null && large.cap > 4096 && large.cap < 20000, String(large.cap));
    assert.match(large.errors, new RegExp(`options: \\[--max-old-space-size=${large.cap}\\]`));
    const small = run(2048);
    assert.strictEqual(small.cap, null);
    assert.match(small.errors, /options: \[\]/);
  });

  it('non-zero exit WITH parseable probe diagnostics stays the incompatible path', async () => {
    // Fake tsc emits a real assignment-class diagnostic for pair-a's probe
    // and exits 1 exactly like the real tsc does on ordinary type errors.
    // The abnormal-termination guard must not swallow it.
    const plan = buildProbe(PAIRS[0], (s) => `@carrick/${s}`);
    const probeFile = `packages/carrick-probes/probes/${plan.fileName}`;
    const tscPath = fakeTsc(
      'tsc-type-error',
      `echo "${probeFile}(${decisiveAssignmentLine(plan)},7): error TS2322: Type 'A' is not assignable to type 'B'."; exit 1`
    );
    const result = await runCheck({ stubs, pairs: PAIRS, tscPath });
    assert.strictEqual(result.success, true);
    const verdictA = result.verdicts.find((v) => v.pair_key === 'pair-a')!;
    assert.strictEqual(verdictA.bucket, 'incompatible', JSON.stringify(verdictA));
    const verdictB = result.verdicts.find((v) => v.pair_key === 'pair-b')!;
    assert.strictEqual(verdictB.bucket, 'compatible', JSON.stringify(verdictB));
  });

  // TypeScript 7 refuses a tsconfig option it removed, prints the refusal at a
  // position in the tsconfig, and checks nothing. That diagnostic belongs to no
  // pair, so before this guard every pair read compatible.
  it('a compiler that refuses its configuration clears no pair', async () => {
    const tscPath = fakeTsc(
      'tsc-config',
      "echo \"packages/carrick-probes/tsconfig.json(19,24): error TS5108: Option 'esModuleInterop=false' has been removed. Please remove it from your configuration.\"; exit 1"
    );
    const result = await runCheck({ stubs, pairs: PAIRS, tscPath });
    assert.strictEqual(result.success, false);
    assert.ok(result.verdicts.length > 0);
    for (const v of result.verdicts) {
      assert.strictEqual(v.bucket, 'unverifiable', JSON.stringify(v));
      assert.strictEqual(v.gate, 'tsc:abnormal-termination');
      assert.match(v.diagnostic ?? '', /TS5108/);
    }
  });

  // The same with the real compilers: a tsconfig carrying an option neither
  // knows. TypeScript 6 reports it and checks the probes anyway; neither run
  // is a check of the configuration the judge wrote.
  for (const [label, real] of [
    ['TypeScript 6', () => resolveVendored(path.join('typescript', 'bin', 'tsc'))],
    ['the native judge', () => judgeTscPath()],
  ] as const) {
    it(`${label}, handed a tsconfig it rejects, clears no pair`, async () => {
      const tscPath = fakeTsc(
        `tsc-bad-config-${label.replace(/\W+/g, '-')}`,
        [
          'cfg=packages/carrick-probes/tsconfig.json',
          'sed \'s/"compilerOptions": {/"compilerOptions": { "notARealOption": true,/\' "$cfg" > "$cfg.tmp" && mv "$cfg.tmp" "$cfg"',
          `exec "${real()}" "$@"`,
        ].join('\n')
      );
      const result = await runCheck({ stubs, pairs: PAIRS, tscPath });
      assert.strictEqual(result.success, false, JSON.stringify(result.verdicts));
      for (const v of result.verdicts) {
        assert.strictEqual(v.bucket, 'unverifiable', JSON.stringify(v));
        assert.strictEqual(v.gate, 'tsc:abnormal-termination');
        assert.match(v.diagnostic ?? '', /TS5023/);
      }
    });
  }

  it('the native compiler out of memory is a heap death, and says why', async () => {
    const tscPath = fakeTsc(
      'tsc-go-oom',
      "echo 'fatal error: runtime: out of memory' >&2; exit 2"
    );
    const result = await runCheck({ stubs, pairs: PAIRS, tscPath });
    assert.strictEqual(result.success, false);
    assert.strictEqual(result.compiler_end?.heap_limit, true);
    assert.strictEqual(result.compiler_end?.exit_code, 2);
    for (const v of result.verdicts) {
      assert.strictEqual(v.bucket, 'unverifiable', JSON.stringify(v));
      assert.match(v.diagnostic ?? '', /ran out of memory/);
    }
  });

  it('a host with no native compiler installed is not checked, and says why', async () => {
    const tscPath = fakeTsc(
      'tsc-no-platform',
      "echo 'Error: Unable to resolve @typescript/typescript-linux-x64. Either your platform is unsupported, or you are missing the package on disk.' >&2; exit 1"
    );
    const result = await runCheck({ stubs, pairs: PAIRS, tscPath });
    assert.strictEqual(result.success, false);
    for (const v of result.verdicts) {
      assert.strictEqual(v.bucket, 'unverifiable', JSON.stringify(v));
      assert.match(v.diagnostic ?? '', /native type checker for this platform is not installed/);
    }
  });
});

describe('check_v2: heap-death evidence and the compiler heap cap', () => {
  // One table for this rule and the scanner's own (`is_heap_limit_line`,
  // type_sidecar.rs), which reads the same file (carrick#2208).
  const table = JSON.parse(
    fs.readFileSync(
      path.join(path.dirname(fileURLToPath(import.meta.url)), '..', '..', 'test', 'fixtures', 'heap-limit-lines.json'),
      'utf8'
    )
  ) as { heap: string[]; not_heap: string[] };

  it('every heap-exhaustion line V8 prints is a heap death', () => {
    assert.ok(table.heap.length >= 4);
    for (const line of table.heap) {
      assert.strictEqual(carriesHeapLimitLine(`<--- Last few GCs --->\n${line}\n 1: 0x1 node`), true, line);
    }
  });

  it('a hard size limit, or no line at all, is not', () => {
    assert.ok(table.not_heap.length >= 3);
    for (const line of table.not_heap) {
      assert.strictEqual(carriesHeapLimitLine(line), false, line);
    }
  });

  it("the child gets the sidecar's cap less what the sidecar holds, only above V8's default", () => {
    assert.deepStrictEqual(childHeap(['--max-old-space-size=6144'], undefined, 3000), {
      nodeOptions: undefined,
      heapCapMb: null,
    });
    assert.deepStrictEqual(childHeap(['--max-old-space-size=8192'], undefined, 2000), {
      nodeOptions: '--max-old-space-size=6192',
      heapCapMb: 6192,
    });
    assert.deepStrictEqual(childHeap(['--max-old-space-size=8192'], '--enable-source-maps', 2000), {
      nodeOptions: '--enable-source-maps --max-old-space-size=6192',
      heapCapMb: 6192,
    });
    assert.deepStrictEqual(childHeap(['--max_old_space_size', '8192'], '', 1000.6), {
      nodeOptions: '--max-old-space-size=7191',
      heapCapMb: 7191,
    });
    assert.deepStrictEqual(childHeap(['--max-old-space-size=8192'], '--x', 4096), {
      nodeOptions: '--x',
      heapCapMb: null,
    });
    assert.deepStrictEqual(childHeap([], '--enable-source-maps', 100), {
      nodeOptions: '--enable-source-maps',
      heapCapMb: null,
    });
  });

  it("the judge runs the host's native executable where the walk finds it, else the launcher", () => {
    const layout = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-judge-tsc-'));
    try {
      const sidecar = path.join(layout, 'node_modules', 'carrick', 'sidecar');
      fs.mkdirSync(sidecar, { recursive: true });
      const launcher = path.join(layout, 'node_modules', 'typescript-native', 'bin', 'tsc');
      fs.mkdirSync(path.dirname(launcher), { recursive: true });
      fs.writeFileSync(launcher, '');
      assert.strictEqual(judgeTscPath(sidecar, 'linux', 'x64'), launcher);
      const native = path.join(layout, 'node_modules', '@typescript', 'typescript-linux-x64', 'lib', 'tsc');
      fs.mkdirSync(path.dirname(native), { recursive: true });
      fs.writeFileSync(native, '');
      assert.strictEqual(judgeTscPath(sidecar, 'linux', 'x64'), native);
      assert.strictEqual(judgeTscPath(sidecar, 'darwin', 'arm64'), launcher);
    } finally {
      fs.rmSync(layout, { recursive: true, force: true });
    }
  });
});
