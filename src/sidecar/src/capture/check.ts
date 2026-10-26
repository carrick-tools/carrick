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
  planProbes,
  planWorkspace,
  writeWorkspace,
  type AssembledWorkspace,
} from './check-workspace.js';
import { JUDGE_BUILD_INFO } from './kept-judge.js';
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
}

function runProcess(
  command: string,
  args: string[],
  cwd: string,
  env?: NodeJS.ProcessEnv
): Promise<SpawnResult> {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd, env, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (d: Buffer) => (stdout += d.toString()));
    child.stderr.on('data', (d: Buffer) => (stderr += d.toString()));
    child.on('error', reject);
    child.on('close', (code, signal) => resolve({ code, signal, stdout, stderr }));
  });
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
  const local = path.join(from, 'node_modules', '.bin', name);
  for (let dir = from; ; dir = path.dirname(dir)) {
    const candidate = path.join(dir, 'node_modules', '.bin', name);
    if (fs.existsSync(candidate)) return candidate;
    if (path.dirname(dir) === dir) return local;
  }
}

function binPath(name: string): string {
  return resolveVendoredBin(name);
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
  const tscPath = opts.tscPath ?? binPath('tsc');
  const tsVersion = ts.version;

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
  const layout = planWorkspace(opts.stubs);

  // Generate probes. A pair naming a service with no stub is unverifiable.
  const aliasRecords = readStubAliasRecords(opts.stubs);
  const plans: ProbePlan[] = [];
  const unresolved: CheckVerdict[] = [];
  for (const spec of opts.pairs) {
    try {
      plans.push(
        buildProbe(spec, layout.packageOf, unwidenedAliasIn(aliasRecords), absenceToleratedIn(aliasRecords))
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
  planProbes(layout, probing);

  // A kept workspace (carrick#2883) is brought to this plan; a fresh one is
  // written out. Either way the files on disk are the plan's.
  const taken = opts.kept?.take(layout, opts.workspaceRoot) ?? null;
  const ws = taken?.ws ?? writeWorkspace(layout, opts.workspaceRoot);
  const kept = taken !== null;
  const judge = kept ? { kept: true as const, install_skipped: taken.installSkipped } : undefined;
  // Whatever happens below, a kept workspace is released: kept when the
  // check finished, dropped when it did not.
  let settled = false;
  const settle = (ok: boolean, installed: boolean): void => {
    if (kept && !settled) opts.kept!.settle(ok, installed);
    settled = true;
  };
  const dispose = (): void => {
    if (!kept && cleanup) safeRm(ws);
  };
  const workspaceDirOut = kept || cleanup ? '' : ws.workspaceDir;
  try {
    const scrubCtx: ScrubContext = {
      workspaceRoot: ws.workspaceDir,
      workspaceRealRoot: fs.realpathSync(ws.workspaceDir),
      packageLabelOf: ws.packageLabelOf,
    };

    const errors: string[] = [];
    const degraded: DegradedService[] = [];

    // ---- Install (async, off the event loop) --------------------------------
    progress('installing', 'installing pinned dependencies');
    const install = taken?.installSkipped
      ? { code: 0, signal: null, stdout: '', stderr: '' }
      : await runProcess(pnpmPath, ['install'], ws.workspaceDir);
    if (install.code !== 0) {
      const installError = scrubPaths(
        (install.stderr || install.stdout).trim().slice(0, 2000),
        scrubCtx
      );
      // `errors` says why the run failed (carrick#1821); each pair the install
      // stopped says it too, because the client reads every pair's own verdict
      // on a failed check (carrick#1833).
      errors.push(`workspace dependency install failed${installError ? `: ${installError}` : ''}`);
      for (const s of opts.stubs) {
        degraded.push({ service_name: s.service_name, reason: 'workspace install failed' });
      }
      const verdicts = sortVerdicts([
        ...unverifiableAll(
          probing,
          'install:failed',
          'workspace dependency install failed; compatibility cannot be verified.' +
            (installError ? ` Installer output: ${installError}` : '')
        ),
        // Capture-decay verdicts stand regardless of the install outcome.
        ...preGated,
        ...unresolved,
      ]);
      settle(false, false);
      dispose();
      return {
        success: false,
        workspace_dir: workspaceDirOut,
        isolation: 'pnpm',
        install_ok: false,
        ts_version: tsVersion,
        verdicts,
        degraded_services: degraded,
        errors,
        ...(judge ? { judge } : {}),
      };
    }

    // ---- Judge (async, off the event loop) ----------------------------------
    progress('checking', 'type-checking probes');
    const heap = childHeap(
      process.execArgv,
      process.env.NODE_OPTIONS,
      process.memoryUsage().rss / (1024 * 1024)
    );
    // On a kept workspace the compiler keeps its build info there and checks
    // again only what changed since the last check (carrick#2883).
    const incremental = kept
      ? ['--incremental', '--tsBuildInfoFile', path.join(ws.workspaceDir, JUDGE_BUILD_INFO)]
      : [];
    const tsc = await runProcess(
      tscPath,
      ['--noEmit', '--pretty', 'false', '-p', path.join(ws.probesRel, 'tsconfig.json'), ...incremental],
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
    if (tsc.signal !== null || (tsc.code !== 0 && diagnostics.length === 0)) {
      const excerpt = scrubPaths(
        (tsc.stderr || tsc.stdout).trim().slice(0, 2000),
        scrubCtx
      );
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
          `the type checker terminated abnormally (exit code ${tsc.code ?? 'null'}); ` +
            'compatibility cannot be verified.' +
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
        heap_limit: carriesHeapLimitLine(tsc.stderr),
        heap_cap_mb: heap.heapCapMb,
      };
      settle(false, true);
      dispose();
      return {
        success: false,
        workspace_dir: workspaceDirOut,
        isolation: 'pnpm',
        install_ok: true,
        ts_version: tsVersion,
        verdicts,
        degraded_services: dedupeDegraded(degraded),
        errors,
        compiler_end: compilerEnd,
        ...(judge ? { judge } : {}),
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

    settle(true, true);
    dispose();

    return {
      success: true,
      workspace_dir: workspaceDirOut,
      isolation: 'pnpm',
      install_ok: true,
      ts_version: tsVersion,
      verdicts,
      degraded_services: dedupeDegraded(degraded),
      errors,
      ...(judge ? { judge } : {}),
    };
  } finally {
    // A throw part way leaves nothing kept that the check did not finish.
    settle(false, false);
  }
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
