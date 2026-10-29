/**
 * v2 check core: "tsc as the judge"
 * (docs/reference/type-compat-synthetic-monorepo.md, Check phase).
 *
 * Consumes two or more capture stub packages, assembles a scratch synthetic
 * monorepo, installs pinned deps with the vendored pnpm (isolated linker),
 * generates one probe per matched pair, runs the vendored `tsc` CLI, and
 * classifies diagnostics into the four buckets. Deterministic given the stubs
 * and pairs: verdicts are byte-stable across runs (the scrub pass removes the
 * per-run temp path, and the pair IDs / ordering derive from the pair specs).
 *
 * Seam: this file imports only node builtins, `typescript` (for the version
 * string), and the rest of this bundle. The sidecar reaches it only through
 * ./index.js. The judge is the `tsc` CLI (no compiler API), which keeps the
 * check phase TS7-ready by construction.
 */

import { spawn } from 'node:child_process';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import ts from 'typescript';
import type {
  CaptureAliasRecord,
  CheckPairEndpoint,
  CheckOptions,
  CompilerEnd,
  CheckProgressPhase,
  CheckResult,
  CheckStubInput,
  CheckVerdict,
  DegradedService,
} from './api.js';
import { buildProbe, unwidenedAliasOf, type ProbePlan } from './check-probe.js';
import {
  classifyPair,
  parseTscOutput,
  type RawDiagnostic,
} from './check-classify.js';
import { scrubPaths, type ScrubContext } from './check-scrub.js';
import {
  assembleWorkspace,
  writeProbes,
  type AssembledWorkspace,
} from './check-workspace.js';
import { buildPoisonIndexes, type StubPoisonIndex } from './check-poison.js';
import { openProbeProgram, probeDeepFindings } from './check-deep.js';
import { onlyDeclaredOpenAny } from './deep-walk.js';
import { pairFieldReports } from './check-fields.js';
import { pairDispatchUnions } from './check-union.js';

export type CheckProgress = (phase: CheckProgressPhase, message: string) => void;

interface SpawnResult {
  code: number | null;
  /** The signal that ended the process, when one did. */
  signal: NodeJS.Signals | null;
  stdout: string;
  stderr: string;
  /** Which time limit killed the process, when one did. */
  timedOut: false | 'total' | 'idle';
}

/** Time limits for one child: a total cap, and a limit on silence. */
interface ProcessLimits {
  totalMs?: number;
  idleMs?: number;
}

function runProcess(
  command: string,
  args: string[],
  cwd: string,
  env?: NodeJS.ProcessEnv,
  limits?: ProcessLimits
): Promise<SpawnResult> {
  return new Promise((resolve, reject) => {
    const limited = limits?.totalMs !== undefined || limits?.idleMs !== undefined;
    // With a limit the child leads its own process group, so the kill takes
    // the package manager's workers with it and `close` can fire.
    const child = spawn(command, args, {
      cwd,
      env,
      stdio: ['ignore', 'pipe', 'pipe'],
      detached: limited,
    });
    let stdout = '';
    let stderr = '';
    let timedOut: SpawnResult['timedOut'] = false;
    const kill = (kind: 'total' | 'idle') => {
      timedOut = kind;
      try {
        process.kill(-(child.pid as number), 'SIGKILL');
      } catch {
        child.kill('SIGKILL');
      }
    };
    const totalTimer =
      limits?.totalMs === undefined ? undefined : setTimeout(() => kill('total'), limits.totalMs);
    let idleTimer: NodeJS.Timeout | undefined;
    const armIdle = () => {
      if (limits?.idleMs === undefined) return;
      if (idleTimer) clearTimeout(idleTimer);
      idleTimer = setTimeout(() => kill('idle'), limits.idleMs);
    };
    const clearTimers = () => {
      if (totalTimer) clearTimeout(totalTimer);
      if (idleTimer) clearTimeout(idleTimer);
    };
    armIdle();
    child.stdout.on('data', (d: Buffer) => {
      stdout += d.toString();
      armIdle();
    });
    child.stderr.on('data', (d: Buffer) => {
      stderr += d.toString();
      armIdle();
    });
    child.on('error', (e) => {
      clearTimers();
      reject(e);
    });
    child.on('close', (code, signal) => {
      clearTimers();
      resolve({ code, signal, stdout, stderr, timedOut });
    });
  });
}

/**
 * The check install's limits (carrick#3021). A healthy install prints
 * progress about once a second (longest gap measured: 1 s), so silence is the
 * sign of a hang: the install is killed after `DEFAULT_INSTALL_IDLE_MS` with
 * no output, and a total cap bounds one that keeps printing. Both are far
 * above the slowest healthy run measured (54 s, install plus judge).
 * `CARRICK_CHECK_INSTALL_TIMEOUT_MS` replaces the total cap.
 */
export const DEFAULT_INSTALL_IDLE_MS = 120_000;
export const DEFAULT_INSTALL_TIMEOUT_MS = 1_800_000;

/** The total cap: the option, else the environment, else the default. */
export function installTotalLimitMs(opt: number | undefined, env: NodeJS.ProcessEnv = process.env): number {
  if (opt !== undefined) return opt;
  const fromEnv = Number(env.CARRICK_CHECK_INSTALL_TIMEOUT_MS);
  return Number.isFinite(fromEnv) && fromEnv > 0 ? fromEnv : DEFAULT_INSTALL_TIMEOUT_MS;
}

/** What the package manager prints when it cannot reach the registry. */
const REGISTRY_UNREACHABLE =
  /ERR_PNPM_META_FETCH_FAIL|ERR_PNPM_FETCH_|ENOTFOUND|ECONNREFUSED|ECONNRESET|ETIMEDOUT|EAI_AGAIN|ENETUNREACH/;

/**
 * Why the check install did not finish, as a gate and a sentence the verdicts
 * carry. A hang and a refused connection are named apart from a plain failure.
 */
export function installFailure(
  install: Pick<SpawnResult, 'timedOut' | 'stderr' | 'stdout'>,
  limits: { idleMs: number; totalMs: number }
): { gate: string; reason: string } {
  if (install.timedOut) {
    const idle = install.timedOut === 'idle';
    return {
      gate: 'install:timeout',
      reason: idle
        ? `workspace dependency install printed nothing for ${Math.round(limits.idleMs / 1000)}s and was stopped (registry unreachable or stuck)`
        : `workspace dependency install did not finish within ${Math.round(limits.totalMs / 1000)}s (registry unreachable or too slow)`,
    };
  }
  if (REGISTRY_UNREACHABLE.test(`${install.stderr}\n${install.stdout}`)) {
    return {
      gate: 'install:registry-unreachable',
      reason: 'workspace dependency install failed: the package registry could not be reached',
    };
  }
  return { gate: 'install:failed', reason: 'workspace dependency install failed' };
}

/** The tail V8 prints on every fatal heap-exhaustion line. */
const HEAP_OUT_OF_MEMORY = 'JavaScript heap out of memory';

/**
 * V8's hard size limits print the same tail, and a larger heap does not lift
 * them, so they are not memory deaths.
 */
const HARD_SIZE_LIMITS = ['invalid table size', 'invalid array length', 'Invalid array buffer length'];

/**
 * Whether a process's stderr carries V8's fatal heap-exhaustion line
 * (`Reached heap limit`, `Ineffective mark-compacts near heap limit`, and the
 * other forms all end in `JavaScript heap out of memory`), and not one of the
 * hard size limits that end the same way (carrick#2209).
 *
 * The scanner applies the same rule to the sidecar's own stderr
 * (carrick#2208); the two must stay in step.
 */
export function carriesHeapLimitLine(stderr: string): boolean {
  return stderr
    .split('\n')
    .some(
      (line) =>
        line.includes(HEAP_OUT_OF_MEMORY) &&
        !HARD_SIZE_LIMITS.some((limit) => line.includes(limit))
    );
}

/**
 * Whether the native compiler (TypeScript 7, a Go program) died out of
 * memory. The Go runtime prints `fatal error: runtime: out of memory` and
 * exits 2; V8's line above never appears, and a heap cap passed through
 * `NODE_OPTIONS` reaches only the launcher, not the compiler.
 */
export function carriesNativeOutOfMemoryLine(output: string): boolean {
  return output.split('\n').some((line) => line.includes('fatal error: runtime: out of memory'));
}

/**
 * Whether a diagnostic is about the compiler's configuration rather than a
 * file it checked: one placed in the tsconfig the judge wrote, or "no inputs"
 * (TS18003). Keyed on the file, not on the TS5xxx option range: some of those
 * codes sit in a source file (TS5097, an import ending in `.ts`) and poison
 * only the stub that holds it.
 */
export function isConfigDiagnostic(d: RawDiagnostic): boolean {
  return path.posix.basename(d.file) === 'tsconfig.json' || d.code === 18003;
}

/** Why the compiler ended abnormally, when its output names a known cause. */
export function abnormalCause(output: string): string | undefined {
  if (carriesNativeOutOfMemoryLine(output)) return 'the type checker ran out of memory';
  return undefined;
}

/** V8's default old-space ceiling on a 64-bit host, in MB. */
const V8_DEFAULT_HEAP_CEILING_MB = 4096;

/** What a Node child of the sidecar is started with. */
export interface ChildHeap {
  /** `NODE_OPTIONS` for the child. */
  nodeOptions: string | undefined;
  /** The heap cap passed to the child, in MB, or null when it runs at V8's
   * default. */
  heapCapMb: number | null;
}

/**
 * The heap a Node child of the sidecar (the check's compiler) is given
 * (carrick#2209).
 *
 * The scanner sizes the sidecar's cap to the machine and passes it as a
 * command-line flag, which a child does not inherit, so the child ran at V8's
 * default. Handing it the same cap would overcommit the box: the sidecar's
 * cap is already 3/4 of memory, and two such caps turn a heap error into an
 * OS kill, which the sizing exists to avoid. So the child gets what is left of
 * the sidecar's cap after what the sidecar holds now (`rssMb`), and only when
 * that is more than V8's default; otherwise `NODE_OPTIONS` is left as it is
 * and the child runs at the default, as before.
 */
export function childHeap(
  execArgv: readonly string[],
  existing: string | undefined,
  rssMb: number
): ChildHeap {
  let cap: number | undefined;
  for (let i = 0; i < execArgv.length; i++) {
    const match = /^--max[-_]old[-_]space[-_]size(?:=(.*))?$/.exec(execArgv[i]);
    if (!match) continue;
    const value = match[1] ?? execArgv[i + 1];
    if (value !== undefined && /^\d+$/.test(value)) cap = Number(value);
  }
  const budget = cap === undefined ? 0 : Math.floor(cap - rssMb);
  if (budget <= V8_DEFAULT_HEAP_CEILING_MB) return { nodeOptions: existing, heapCapMb: null };
  const flag = `--max-old-space-size=${budget}`;
  return {
    nodeOptions: existing && existing.trim() ? `${existing} ${flag}` : flag,
    heapCapMb: budget,
  };
}

/** Resolve the sidecar root (where the vendored pnpm/tsc live) from this bundle. */
function sidecarRoot(): string {
  // dist/src/capture/check.js -> up three -> sidecar root.
  const here = path.dirname(fileURLToPath(import.meta.url));
  return path.resolve(here, '..', '..', '..');
}

/**
 * Locate a vendored executable the way node locates a package: walk up from
 * the bundle and take the first `node_modules/.bin/<name>` that exists.
 *
 * Where the bins sit depends on how the sidecar arrived. A checkout and the
 * release tarball both keep them in the sidecar's own `node_modules`, so the
 * first candidate hits and nothing changes. An npm install never can: npm
 * strips nested `node_modules` from a published tarball, so the sidecar ships
 * as a plain directory inside the package and its dependencies are installed
 * by the package that carries it — hoisted to the install root, or nested one
 * level up when a version conflict forces it. Assuming one shape is what made
 * an installed `carrick` degrade every pair to unverifiable (carrick#833).
 *
 * `from` is injectable so the layouts can be pinned by test; production always
 * starts at the bundle. Returns the sidecar-local path when nothing exists,
 * which is the caller's "not found" signal and the most useful thing to name.
 */
export function resolveVendoredBin(name: string, from: string = sidecarRoot()): string {
  return resolveVendored(path.join('.bin', name), from);
}

/**
 * The first `node_modules/<rel>` at or above `from`, the same walk as
 * {@link resolveVendoredBin}; the sidecar-local path when none exists.
 */
export function resolveVendored(rel: string, from: string = sidecarRoot()): string {
  const local = path.join(from, 'node_modules', rel);
  for (let dir = from; ; dir = path.dirname(dir)) {
    const candidate = path.join(dir, 'node_modules', rel);
    if (fs.existsSync(candidate)) return candidate;
    if (path.dirname(dir) === dir) return local;
  }
}

function binPath(name: string): string {
  return resolveVendoredBin(name);
}

/**
 * The package that brings the judge's compiler: TypeScript 7 under an alias
 * beside the TypeScript 6 that capture and inference read through ts-morph.
 * Its platform package holds the native executable the judge runs. Both
 * packages declare a `tsc` bin, so `node_modules/.bin/tsc` may be either; the
 * judge never reads it.
 */
export const JUDGE_COMPILER_PACKAGE = 'typescript-native';

/**
 * The judge's tsc: the host's native executable, from the platform package
 * the compiler installs as an optional dependency (`@typescript/
 * typescript-<platform>-<arch>`), found by the same walk as the bins. Where it
 * is not installed (an install that omits optional dependencies, such as the
 * cloud runner's, or a pnpm store the walk does not reach), TypeScript 6's own
 * tsc, which ships for ts-morph anyway. The two read every probe the same
 * (measured on 1,018 real probes; the one wording difference moves only a
 * verdict's `codes`), so the fallback changes no verdict, only the time.
 */
export function judgeTscPath(
  from: string = sidecarRoot(),
  platform: string = process.platform,
  arch: string = process.arch
): string {
  const exe = platform === 'win32' ? 'tsc.exe' : 'tsc';
  const native = path.join('@typescript', `typescript-${platform}-${arch}`, 'lib', exe);
  const found = resolveVendored(native, from);
  if (fs.existsSync(found)) return found;
  return resolveVendored(path.join('typescript', 'bin', 'tsc'), from);
}

/** Which compiler a tsc path is, in words, for the run log and the result. */
export function judgeLabel(tscPath: string): string {
  const version = compilerVersion(tscPath);
  const parts = tscPath.split(/[\\/]/);
  if (parts.some((p) => p.startsWith('typescript-') && p !== JUDGE_COMPILER_PACKAGE)) {
    return `TypeScript ${version} (native)`;
  }
  if (parts.includes('typescript') && parts[parts.length - 2] === 'bin') {
    return `TypeScript ${version} (native compiler not installed)`;
  }
  return `a stand-in compiler at ${tscPath}`;
}

/** The version of the compiler a tsc path belongs to, read from the package
 * that holds it; the compiler-API version for a stand-in with no package. */
function compilerVersion(tscPath: string): string {
  try {
    const manifest = JSON.parse(
      fs.readFileSync(path.join(path.dirname(tscPath), '..', 'package.json'), 'utf8')
    ) as { name?: string; version?: string };
    const compiler =
      manifest.name === 'typescript' || manifest.name?.startsWith('@typescript/typescript-');
    if (compiler && manifest.version) return manifest.version;
  } catch {
    // A stand-in compiler (tests) sits in no package.
  }
  return ts.version;
}

function unverifiableAll(
  plans: ProbePlan[],
  gate: string,
  diagnostic: string
): CheckVerdict[] {
  return plans.map((plan) => ({
    pair_id: plan.pairId,
    pair_key: plan.spec.pair_key,
    bucket: 'unverifiable' as const,
    gate,
    diagnostic,
    codes: [],
    resolved: false,
    unresolved_reason: diagnostic,
    // Nothing was compared here, so there is nothing to observe about the
    // comparison (carrick#1341). Empty is the honest answer, not a default.
    notes: [],
  }));
}

function sortVerdicts(verdicts: CheckVerdict[]): CheckVerdict[] {
  return [...verdicts].sort((a, b) =>
    a.pair_id < b.pair_id ? -1 : a.pair_id > b.pair_id ? 1 : 0
  );
}

/** Run the deterministic check phase. Async: install + tsc run off the event
 * loop so the sidecar stays responsive and can emit keepalive frames. */
export async function runCheck(
  opts: CheckOptions,
  onProgress?: CheckProgress
): Promise<CheckResult> {
  const progress: CheckProgress = onProgress ?? (() => {});
  const cleanup = opts.cleanup !== false;
  const pnpmPath = opts.pnpmPath ?? binPath('pnpm');
  const tscPath = opts.tscPath ?? judgeTscPath();
  const tsVersion = compilerVersion(tscPath);
  const judge = judgeLabel(tscPath);

  // Isolation is non-negotiable: without the vendored pnpm we would fall back
  // to a flat npm install that manufactures nominal false-incompatibles. Fail
  // explicitly instead (design Check step 2).
  if (!fs.existsSync(pnpmPath)) {
    // Capture-decay verdicts stand without isolation too (carrick#1833).
    const { preGated, probing } = preGate(
      planPairs(opts, (s) => `@carrick/${s}`),
      readStubAliasRecords(opts.stubs)
    );
    return {
      success: false,
      workspace_dir: '',
      isolation: 'unavailable',
      install_ok: false,
      ts_version: tsVersion,
      judge,
      verdicts: sortVerdicts([
        ...unverifiableAll(
          probing,
          'isolation:unavailable',
          'type isolation unavailable (pnpm missing); compatibility cannot be verified.'
        ),
        ...preGated,
      ]),
      degraded_services: opts.stubs.map((s) => ({
        service_name: s.service_name,
        reason: 'isolation unavailable',
      })),
      errors: ['vendored pnpm not found'],
    };
  }

  progress('assembling', 'assembling synthetic workspace');
  const ws = assembleWorkspace({
    stubs: opts.stubs,
    workspaceRoot: opts.workspaceRoot,
  });

  const scrubCtx: ScrubContext = {
    workspaceRoot: ws.workspaceDir,
    workspaceRealRoot: fs.realpathSync(ws.workspaceDir),
    packageLabelOf: ws.packageLabelOf,
  };

  // Generate probes. A pair naming a service with no stub is unverifiable.
  const aliasRecords = readStubAliasRecords(opts.stubs);
  const plans: ProbePlan[] = [];
  const unresolved: CheckVerdict[] = [];
  for (const spec of opts.pairs) {
    try {
      plans.push(
        buildProbe(spec, ws.packageOf, unwidenedAliasIn(aliasRecords), absenceToleratedIn(aliasRecords))
      );
    } catch {
      unresolved.push({
        pair_id: '',
        pair_key: spec.pair_key,
        bucket: 'unverifiable',
        gate: 'missing:stub',
        diagnostic: 'a stub for one side of this pair was not provided; compatibility cannot be verified.',
        codes: [],
        resolved: false,
        unresolved_reason: 'one side of this pair has no captured type surface',
        notes: [],
      });
    }
  }
  // Backfill deterministic pair IDs for unresolved pairs from the spec.
  for (const v of unresolved) {
    const spec = opts.pairs.find((p) => p.pair_key === v.pair_key)!;
    v.pair_id = fnvOfSpec(spec);
  }

  const { preGated, probing } = preGate(plans, aliasRecords);
  writeProbes(ws, probing);

  const errors: string[] = [];
  const degraded: DegradedService[] = [];

  // ---- Install (async, off the event loop) --------------------------------
  progress('installing', 'installing pinned dependencies');
  const installLimits = {
    idleMs: opts.installIdleTimeoutMs ?? DEFAULT_INSTALL_IDLE_MS,
    totalMs: installTotalLimitMs(opts.installTimeoutMs),
  };
  const install = await runProcess(
    pnpmPath,
    ['install'],
    ws.workspaceDir,
    undefined,
    installLimits
  );
  if (install.code !== 0 || install.timedOut) {
    const failure = installFailure(install, installLimits);
    const installError = scrubPaths(
      (install.stderr || install.stdout).trim().slice(0, 2000),
      scrubCtx
    );
    // `errors` says why the run failed (carrick#1821); each pair the install
    // stopped says it too, because the client reads every pair's own verdict
    // on a failed check (carrick#1833).
    errors.push(
      `${failure.reason}${installError && failure.gate !== 'install:timeout' ? `: ${installError}` : ''}`
    );
    for (const s of opts.stubs) {
      degraded.push({ service_name: s.service_name, reason: 'workspace install failed' });
    }
    const verdicts = sortVerdicts([
      ...unverifiableAll(
        probing,
        failure.gate,
        `${failure.reason}; compatibility cannot be verified.` +
          (installError ? ` Installer output: ${installError}` : '')
      ),
      // Capture-decay verdicts stand regardless of the install outcome.
      ...preGated,
      ...unresolved,
    ]);
    if (cleanup) safeRm(ws);
    return {
      success: false,
      workspace_dir: cleanup ? '' : ws.workspaceDir,
      isolation: 'pnpm',
      install_ok: false,
      ts_version: tsVersion,
      judge,
      verdicts,
      degraded_services: degraded,
      errors,
    };
  }

  // ---- Judge (async, off the event loop) ----------------------------------
  progress('checking', `type-checking probes (judge: ${judge})`);
  const heap = childHeap(
    process.execArgv,
    process.env.NODE_OPTIONS,
    process.memoryUsage().rss / (1024 * 1024)
  );
  const tsc = await runProcess(
    tscPath,
    ['--noEmit', '--pretty', 'false', '-p', path.join(ws.probesRel, 'tsconfig.json')],
    ws.workspaceDir,
    { ...process.env, NODE_OPTIONS: heap.nodeOptions }
  );
  const diagnostics = parseTscOutput(tsc.stdout + '\n' + tsc.stderr);

  // Abnormal tsc termination: OOM/SIGKILL (exit code null), an internal
  // crash, or a fileless config error (e.g. TS18003, printed without a
  // (line,col) prefix) all yield ZERO parseable diagnostics — and an empty
  // diagnostic set must never fall through to "compatible". A non-zero exit
  // WITH parsed diagnostics is the legitimate incompatible path (tsc exits 1
  // on ordinary type errors) and stays untouched. Mirrors the install step's
  // `code === 0` guard.
  // A compiler a signal ended never finished, so a pair with no diagnostic
  // was never cleared, whatever it printed first (carrick#2209).
  //
  // A diagnostic about the configuration (a tsconfig the compiler refuses,
  // an option it does not know) is printed WITH a position, in the tsconfig,
  // and belongs to no pair: TypeScript 7 refuses an option and checks
  // nothing, so without this every pair read compatible. Such a run checked
  // no pair, whatever else it printed.
  const configError = diagnostics.find(isConfigDiagnostic);
  if (
    tsc.signal !== null ||
    (tsc.code !== 0 && diagnostics.length === 0) ||
    configError !== undefined
  ) {
    const excerpt = scrubPaths(
      (configError
        ? `${configError.file}(${configError.line},${configError.col}): error TS${configError.code}: ${configError.message}`
        : (tsc.stderr || tsc.stdout).trim()
      ).slice(0, 2000),
      scrubCtx
    );
    const cause = abnormalCause(tsc.stderr + '\n' + tsc.stdout);
    errors.push(
      `tsc terminated abnormally (exit code ${tsc.code ?? 'null'}) ` +
        `with no parseable diagnostics${excerpt ? `: ${excerpt}` : ''}`
    );
    for (const s of opts.stubs) {
      degraded.push({
        service_name: s.service_name,
        reason: 'type checker terminated abnormally',
      });
    }
    const verdicts = sortVerdicts([
      ...unverifiableAll(
        probing,
        'tsc:abnormal-termination',
        `the type checker terminated abnormally (exit code ${tsc.code ?? 'null'})` +
          (cause ? ` because ${cause}` : '') +
          '; compatibility cannot be verified.' +
          (excerpt ? ` Compiler output: ${excerpt}` : '')
      ),
      ...preGated,
      ...unresolved,
    ]);
    // How the compiler ended, as fields, so the scanner can tell a memory
    // death from any other abnormal end without reading the text
    // (carrick#2209).
    const compilerEnd: CompilerEnd = {
      exit_code: tsc.code,
      signal: tsc.signal,
      heap_limit: carriesHeapLimitLine(tsc.stderr) || carriesNativeOutOfMemoryLine(tsc.stderr + '\n' + tsc.stdout),
      heap_cap_mb: heap.heapCapMb,
    };
    if (cleanup) safeRm(ws);
    return {
      success: false,
      workspace_dir: cleanup ? '' : ws.workspaceDir,
      isolation: 'pnpm',
      install_ok: true,
      ts_version: tsVersion,
      judge,
      verdicts,
      degraded_services: dedupeDegraded(degraded),
      errors,
      compiler_end: compilerEnd,
    };
  }

  const { probeDiagsByPair, poison, globalErrors } = attributeDiagnostics(
    diagnostics,
    ws,
    probing
  );
  for (const e of globalErrors) errors.push(scrubPaths(e, scrubCtx));
  // Only SERVICE-WIDE poison (an unattributable stub diagnostic) degrades the
  // whole service; contained alias-scoped poison rides its own verdicts.
  for (const [service, scope] of poison) {
    if (scope.all) degraded.push({ service_name: service, reason: scope.reason });
  }
  const poisonReason = (service: string, alias: string): string | undefined => {
    const scope = poison.get(service);
    if (!scope) return undefined;
    return scope.all || scope.aliases.has(alias) ? scope.reason : undefined;
  };

  // Fact-ness walk (carrick#707, R1d): the probe gates are whole-type only,
  // so a compared pair is only a fact about two known types when neither side
  // carries a member-level any/unknown HERE — after install, where the capture
  // could not look (carrick#450). It sets `resolved` and nothing else; no
  // bucket depends on it.
  const probeProgram = openProbeProgram(ws.probesDir, probing);
  const deepByPair = probeDeepFindings(probeProgram, probing);
  // The same program answers which FIELDS differ on a pair the judge called
  // incompatible (carrick-tools/carrick-cloud#1118). It names what the verdict
  // is about; it never decides one.
  const fieldsByPair = pairFieldReports(probeProgram, probing);
  // And which pairs compared a union the expected side takes only in part,
  // the rest failing by nothing but the weak-type check (carrick#1995).
  const unionsByPair = pairDispatchUnions(probeProgram, probing);

  const verdicts = sortVerdicts([
    ...probing.map((plan) =>
      classifyPair({
        plan,
        probeDiags: probeDiagsByPair.get(plan.pairId) ?? [],
        poisonReason,
        scrubCtx,
        deepFindings: deepByPair.get(plan.pairId),
        fieldReport: fieldsByPair.get(plan.pairId),
        dispatchUnion: unionsByPair.get(plan.pairId),
        rawTextSide: rawTextSideOf(plan, aliasRecords),
      })
    ),
    ...preGated,
    ...unresolved,
  ]);

  const workspaceDirOut = cleanup ? '' : ws.workspaceDir;
  if (cleanup) safeRm(ws);

  return {
    success: true,
    workspace_dir: workspaceDirOut,
    isolation: 'pnpm',
    install_ok: true,
    ts_version: tsVersion,
    judge,
    verdicts,
    degraded_services: dedupeDegraded(degraded),
    errors,
  };
}

/**
 * Per-service alias records from each stub's carrick-manifest.json. Absent
 * or unparseable manifests (e.g. hand-authored stubs in tests) contribute
 * nothing — only positively-recorded deep decay pre-gates a pair.
 */
function readStubAliasRecords(
  stubs: CheckStubInput[]
): Map<string, Map<string, CaptureAliasRecord>> {
  const byService = new Map<string, Map<string, CaptureAliasRecord>>();
  for (const stub of stubs) {
    try {
      const manifest = JSON.parse(
        fs.readFileSync(path.join(stub.stub_dir, 'carrick-manifest.json'), 'utf8')
      ) as { aliases?: CaptureAliasRecord[] };
      const byAlias = new Map<string, CaptureAliasRecord>();
      for (const record of manifest.aliases ?? []) {
        if (record?.alias) byAlias.set(record.alias, record);
      }
      byService.set(stub.service_name, byAlias);
    } catch {
      // No manifest: nothing to pre-gate for this stub.
    }
  }
  return byService;
}

/**
 * The unwidened reading a sending side's stub ships beside one of its aliases
 * (carrick#1538, carrick#2011), as `buildProbe` asks for it. Read from the same manifest the
 * pre-gate reads. A reading the capture already found a member-level
 * `any`/`unknown` in is not offered: it would assign to whatever the other
 * side declares.
 */
function unwidenedAliasIn(
  aliasRecords: Map<string, Map<string, CaptureAliasRecord>>
): (sent: CheckPairEndpoint) => string | undefined {
  return (sent) => {
    const record = aliasRecords
      .get(sent.service_name)
      ?.get(unwidenedAliasOf(sent.alias));
    if (!record || record.any_provenance?.length) return undefined;
    return record.alias;
  };
}

/**
 * carrick#2814: whether the receiving side's capture record marks a handler
 * that works when no body is sent.
 */
function absenceToleratedIn(
  aliasRecords: Map<string, Map<string, CaptureAliasRecord>>
): (expected: CheckPairEndpoint) => boolean {
  return (expected) =>
    aliasRecords.get(expected.service_name)?.get(expected.alias)?.absent_body_tolerated === true;
}

interface DeepDecayHit {
  side: 'producer' | 'consumer';
  kind: 'any' | 'unknown' | 'budget_exhausted';
  path: string;
}

/**
 * Capture-time deep-decay pre-gate (adversarial-review finding 1): a
 * member-level `any`/`unknown` recorded by the capture self-check with no
 * failing-external explanation. The probe gates are WHOLE-type only —
 * `{ orderId: string; metadata: any }` sails through IsAny and the assignment
 * compiles clean — so such pairs must never reach a probe. `any` routes to
 * gate_caught_baked_any, `unknown` to unverifiable; both read as None
 * downstream, never compatible. These verdicts come from the capture alone,
 * so they stand whatever the install or the compiler then does (carrick#1833).
 */
function preGate(
  plans: ProbePlan[],
  aliasRecords: Map<string, Map<string, CaptureAliasRecord>>
): { preGated: CheckVerdict[]; probing: ProbePlan[] } {
  const preGated: CheckVerdict[] = [];
  const probing: ProbePlan[] = [];
  for (const plan of plans) {
    const hit = deepDecayOf(plan, aliasRecords);
    if (!hit) {
      probing.push(plan);
      continue;
    }
    preGated.push({
      pair_id: plan.pairId,
      pair_key: plan.spec.pair_key,
      // `any` is a confirmed baked top type -> gate_caught_baked_any; `unknown`
      // and `budget_exhausted` (a subtree the capture walk could not finish)
      // are "cannot verify" -> unverifiable. All read as None downstream.
      bucket: hit.kind === 'any' ? 'gate_caught_baked_any' : 'unverifiable',
      gate: `capture:${hit.side}:${hit.kind}`,
      diagnostic:
        hit.kind === 'budget_exhausted'
          ? `the ${hit.side} type is too deep or wide to verify within the ` +
            `capture budget (at '${hit.path}'); compatibility cannot be ` +
            `verified — abstaining so a buried 'any' can never read compatible.`
          : `the ${hit.side} type carries '${hit.kind}' at '${hit.path}' from ` +
            `capture; compatibility cannot be verified (a partially-unresolved ` +
            `type would let an arbitrary shape read compatible).`,
      codes: [],
      resolved: false,
      unresolved_side: hit.side,
      unresolved_reason:
        hit.kind === 'budget_exhausted'
          ? `the ${hit.side} type is too deep or wide to verify at '${hit.path}'`
          : `the ${hit.side} type carries '${hit.kind}' at '${hit.path}'`,
      notes: [],
    });
  }
  return { preGated, probing };
}

/**
 * The side whose capture record marks its body as read raw (carrick#1842):
 * the sent side when both are, as the bytes gate names it.
 */
function rawTextSideOf(
  plan: ProbePlan,
  aliasRecords: Map<string, Map<string, CaptureAliasRecord>>
): 'producer' | 'consumer' | undefined {
  return [plan.direction.sent, plan.direction.expected].find((side) => {
    const endpoint = plan.spec[side];
    return aliasRecords.get(endpoint.service_name)?.get(endpoint.alias)?.raw_text_read === true;
  });
}

/** First side (producer, then consumer) whose capture recorded a deep decay. */
function deepDecayOf(
  plan: ProbePlan,
  aliasRecords: Map<string, Map<string, CaptureAliasRecord>>
): DeepDecayHit | undefined {
  for (const side of ['producer', 'consumer'] as const) {
    const endpoint = plan.spec[side];
    const record = aliasRecords.get(endpoint.service_name)?.get(endpoint.alias);
    // The capture emits every deep finding it made; the FIRST is the one the
    // pre-gate reads, exactly as when the walk stopped at the first hit.
    const findings = record?.any_provenance ?? [];
    // An `any` the source declares under a named member is an open member
    // (carrick#2591): the pair is probed, and the compiler judges the closed
    // members. Anything else on the record keeps the pair out of the probe.
    if (onlyDeclaredOpenAny(findings)) continue;
    const finding = findings[0];
    if (finding) {
      return { side, kind: finding.kind, path: finding.path };
    }
  }
  return undefined;
}

/**
 * Alias-scoped poison for a service (#438 part 2). `all` is set only when a
 * stub diagnostic could not be attributed to any alias's closure (soundness
 * fallback); otherwise exactly the reachable aliases are poisoned.
 */
interface PoisonScope {
  all: boolean;
  aliases: Set<string>;
  reason: string;
}

const POISON_REASON = 'stub type tree carries its own diagnostics';

/**
 * Group diagnostics: per-probe (for classification), plus stub-tree poison
 * CONTAINED to the aliases reachable from the poisoned file (#438 part 2). An
 * unattributable stub diagnostic (an augmentation-file collision in no alias's
 * closure) falls back to service-wide poison; everything else is scoped.
 */
function attributeDiagnostics(
  diagnostics: RawDiagnostic[],
  ws: AssembledWorkspace,
  plans: ProbePlan[]
): {
  probeDiagsByPair: Map<string, RawDiagnostic[]>;
  poison: Map<string, PoisonScope>;
  globalErrors: string[];
} {
  const probeFileToPair = new Map<string, string>();
  for (const plan of plans) {
    probeFileToPair.set(`${ws.probesRel}/probes/${plan.fileName}`, plan.pairId);
  }
  const stubDirs = new Set(ws.stubs.map((s) => s.packageDir));
  // Lazy: the clean path (no stub-tree diagnostic) must not walk and parse
  // every stub `.d.ts` tree. Built on first stub-tree diagnostic, at most once.
  let indexesMemo: Map<string, StubPoisonIndex> | undefined;
  const getIndexes = (): Map<string, StubPoisonIndex> =>
    (indexesMemo ??= buildPoisonIndexes(ws));

  const probeDiagsByPair = new Map<string, RawDiagnostic[]>();
  const poison = new Map<string, PoisonScope>();
  const globalErrors: string[] = [];

  const scopeFor = (service: string): PoisonScope => {
    let scope = poison.get(service);
    if (!scope) {
      scope = { all: false, aliases: new Set(), reason: POISON_REASON };
      poison.set(service, scope);
    }
    return scope;
  };

  for (const d of diagnostics) {
    if (!d.file) {
      globalErrors.push(`TS${d.code}: ${d.message}`);
      continue;
    }
    const pairId = probeFileToPair.get(d.file);
    if (pairId) {
      if (!probeDiagsByPair.has(pairId)) probeDiagsByPair.set(pairId, []);
      probeDiagsByPair.get(pairId)!.push(d);
      continue;
    }
    // Stub-tree diagnostics poison that stub (its OWN sources, not deps).
    const pkgMatch = d.file.match(/^packages\/([^/]+)\//);
    if (pkgMatch && !d.file.includes('/node_modules/') && stubDirs.has(pkgMatch[1])) {
      const service = ws.serviceOfPackageDir(pkgMatch[1]);
      if (!service) continue;
      const scope = scopeFor(service);
      const index = getIndexes().get(service);
      if (index && d.file === index.surfaceFile) {
        // Surface diagnostic: attribute by the alias statement span it sits in.
        const alias = index.aliasAtSurfaceLine(d.line);
        if (alias) scope.aliases.add(alias);
        else scope.all = true;
      } else if (index) {
        // Nested-file diagnostic: attribute to every alias whose closure reads
        // it; a file in no closure (augmentation collision) falls back wide.
        const affected = index.aliasesForFile(d.file);
        if (affected.length > 0) for (const a of affected) scope.aliases.add(a);
        else scope.all = true;
      } else {
        scope.all = true;
      }
    }
    // Anything else (node_modules, TS lib) is environmental: ignored for
    // verdicts so it cannot inject nondeterministic noise.
  }

  return { probeDiagsByPair, poison, globalErrors };
}

function dedupeDegraded(list: DegradedService[]): DegradedService[] {
  const seen = new Map<string, DegradedService>();
  for (const d of list) if (!seen.has(d.service_name)) seen.set(d.service_name, d);
  return [...seen.values()].sort((a, b) =>
    a.service_name < b.service_name ? -1 : a.service_name > b.service_name ? 1 : 0
  );
}

/** Plan pairs without a workspace (for the isolation-unavailable early exit). */
function planPairs(
  opts: CheckOptions,
  packageOf: (serviceName: string) => string
): ProbePlan[] {
  const plans: ProbePlan[] = [];
  for (const spec of opts.pairs) {
    try {
      plans.push(buildProbe(spec, packageOf));
    } catch {
      /* skipped in the early-exit path */
    }
  }
  return plans;
}

function fnvOfSpec(spec: CheckOptions['pairs'][number]): string {
  return buildProbe(spec, (s) => `@carrick/${s}`).pairId;
}

function safeRm(ws: AssembledWorkspace): void {
  try {
    ws.guard.remove(ws.workspaceDir);
  } catch {
    /* best effort */
  }
}
