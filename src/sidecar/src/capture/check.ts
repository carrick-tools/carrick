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
  CheckOptions,
  CheckProgressPhase,
  CheckResult,
  CheckStubInput,
  CheckVerdict,
  DegradedService,
} from './api.js';
import { buildProbe, type ProbePlan } from './check-probe.js';
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
import { probeDeepFindings } from './check-deep.js';

export type CheckProgress = (phase: CheckProgressPhase, message: string) => void;

interface SpawnResult {
  code: number | null;
  stdout: string;
  stderr: string;
}

function runProcess(
  command: string,
  args: string[],
  cwd: string
): Promise<SpawnResult> {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (d: Buffer) => (stdout += d.toString()));
    child.stderr.on('data', (d: Buffer) => (stderr += d.toString()));
    child.on('error', reject);
    child.on('close', (code) => resolve({ code, stdout, stderr }));
  });
}

/** Resolve the sidecar root (where the vendored pnpm/tsc live) from this bundle. */
function sidecarRoot(): string {
  // dist/src/capture/check.js -> up three -> sidecar root.
  const here = path.dirname(fileURLToPath(import.meta.url));
  return path.resolve(here, '..', '..', '..');
}

function binPath(name: string): string {
  return path.join(sidecarRoot(), 'node_modules', '.bin', name);
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
    const planned = planPairs(opts, (s) => `@carrick/${s}`);
    return {
      success: false,
      workspace_dir: '',
      isolation: 'unavailable',
      install_ok: false,
      install_error: 'vendored pnpm not found; type isolation is unavailable',
      ts_version: tsVersion,
      verdicts: sortVerdicts(
        unverifiableAll(
          planned,
          'isolation:unavailable',
          'type isolation unavailable (pnpm missing); compatibility cannot be verified.'
        )
      ),
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
    packageLabelOf: ws.packageLabelOf,
  };

  // Generate probes. A pair naming a service with no stub is unverifiable.
  const plans: ProbePlan[] = [];
  const unresolved: CheckVerdict[] = [];
  for (const spec of opts.pairs) {
    try {
      plans.push(buildProbe(spec, ws.packageOf));
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
      });
    }
  }
  // Backfill deterministic pair IDs for unresolved pairs from the spec.
  for (const v of unresolved) {
    const spec = opts.pairs.find((p) => p.pair_key === v.pair_key)!;
    v.pair_id = fnvOfSpec(spec);
  }

  // Capture-time deep-decay pre-gate (adversarial-review finding 1): a
  // member-level `any`/`unknown` recorded by the capture self-check with no
  // failing-external explanation. The probe gates below are WHOLE-type only
  // — `{ orderId: string; metadata: any }` sails through IsAny and the
  // assignment compiles clean — so such pairs must never reach a probe.
  // `any` routes to gate_caught_baked_any, `unknown` to unverifiable; both
  // read as None downstream, never compatible.
  const aliasRecords = readStubAliasRecords(opts.stubs);
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
      unresolved_reason:
        hit.kind === 'budget_exhausted'
          ? `the ${hit.side} type is too deep or wide to verify at '${hit.path}'`
          : `the ${hit.side} type carries '${hit.kind}' at '${hit.path}'`,
    });
  }
  writeProbes(ws, probing);

  const errors: string[] = [];
  const degraded: DegradedService[] = [];

  // ---- Install (async, off the event loop) --------------------------------
  progress('installing', 'installing pinned dependencies');
  const install = await runProcess(pnpmPath, ['install'], ws.workspaceDir);
  const installOk = install.code === 0;
  let installError: string | undefined;
  if (!installOk) {
    installError = scrubPaths(
      (install.stderr || install.stdout).trim().slice(0, 2000),
      scrubCtx
    );
    for (const s of opts.stubs) {
      degraded.push({ service_name: s.service_name, reason: 'workspace install failed' });
    }
    const verdicts = sortVerdicts([
      ...unverifiableAll(
        probing,
        'install:failed',
        'workspace dependency install failed; compatibility cannot be verified.'
      ),
      // Capture-decay verdicts stand regardless of the install outcome.
      ...preGated,
      ...unresolved,
    ]);
    if (cleanup) safeRm(ws.workspaceDir);
    return {
      success: false,
      workspace_dir: cleanup ? '' : ws.workspaceDir,
      isolation: 'pnpm',
      install_ok: false,
      install_error: installError,
      ts_version: tsVersion,
      verdicts,
      degraded_services: degraded,
      errors,
    };
  }

  // ---- Judge (async, off the event loop) ----------------------------------
  progress('checking', 'type-checking probes');
  const tsc = await runProcess(
    tscPath,
    ['--noEmit', '--pretty', 'false', '-p', path.join(ws.probesRel, 'tsconfig.json')],
    ws.workspaceDir
  );
  const diagnostics = parseTscOutput(tsc.stdout + '\n' + tsc.stderr);

  // Abnormal tsc termination: OOM/SIGKILL (exit code null), an internal
  // crash, or a fileless config error (e.g. TS18003, printed without a
  // (line,col) prefix) all yield ZERO parseable diagnostics — and an empty
  // diagnostic set must never fall through to "compatible". A non-zero exit
  // WITH parsed diagnostics is the legitimate incompatible path (tsc exits 1
  // on ordinary type errors) and stays untouched. Mirrors the install step's
  // `code === 0` guard.
  if (tsc.code !== 0 && diagnostics.length === 0) {
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
        'the type checker terminated abnormally; compatibility cannot be verified.'
      ),
      ...preGated,
      ...unresolved,
    ]);
    if (cleanup) safeRm(ws.workspaceDir);
    return {
      success: false,
      workspace_dir: cleanup ? '' : ws.workspaceDir,
      isolation: 'pnpm',
      install_ok: true,
      ts_version: tsVersion,
      verdicts,
      degraded_services: dedupeDegraded(degraded),
      errors,
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
  const deepByPair = probeDeepFindings(ws.probesDir, probing);

  const verdicts = sortVerdicts([
    ...probing.map((plan) =>
      classifyPair({
        plan,
        probeDiags: probeDiagsByPair.get(plan.pairId) ?? [],
        poisonReason,
        scrubCtx,
        deepFindings: deepByPair.get(plan.pairId),
      })
    ),
    ...preGated,
    ...unresolved,
  ]);

  const workspaceDirOut = cleanup ? '' : ws.workspaceDir;
  if (cleanup) safeRm(ws.workspaceDir);

  return {
    success: true,
    workspace_dir: workspaceDirOut,
    isolation: 'pnpm',
    install_ok: true,
    ts_version: tsVersion,
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

interface DeepDecayHit {
  side: 'producer' | 'consumer';
  kind: 'any' | 'unknown' | 'budget_exhausted';
  path: string;
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
    const finding = record?.any_provenance?.[0];
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

function safeRm(dir: string): void {
  try {
    fs.rmSync(dir, { recursive: true, force: true });
  } catch {
    /* best effort */
  }
}
