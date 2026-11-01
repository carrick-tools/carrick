/**
 * Type-compat v2 capture core: "tsc as the serializer"
 * (docs/reference/type-compat-synthetic-monorepo.md, Capture phase).
 *
 * Produces a types-only stub package for one service:
 *
 *   @carrick/<service>/
 *   |- package.json            name, types entry, pinned deps (exact versions)
 *   |- tsconfig.snapshot.json
 *   |- carrick-manifest.json   per-alias records + fidelity metric
 *   `- types/
 *      |- surface.d.ts         entry: export type <alias> = ...
 *      `- nested .d.ts tree    compiler-emitted declaration closure
 *
 * Two-phase flow:
 *   Phase A (analysis): a placeholder surface entry + the anchors' source
 *     files form a program; addressable anchors run their guards, anonymous
 *     anchors are located and printed via the SymbolTracker-backed node
 *     builder (anchored at their placeholder -- the destination file).
 *   Phase B (emit): the final entry runs `tsc --noCheck --declaration
 *     --emitDeclarationOnly` with the repo's own parsed options, plus every
 *     detected augmentation file as an extra root; the tree is relocated
 *     into the stub, specifiers are rewritten, deps pinned, and the
 *     per-alias self-check classifies the result.
 *
 * Seam note: this directory is the whole v2 capture bundle. It imports only
 * node builtins and `typescript`; the rest of the sidecar reaches it only
 * through ./api.js types and this file's `captureStub`.
 */

import ts from 'typescript';
import * as fs from 'node:fs';
import * as path from 'node:path';
import type {
  AnchorOrigin,
  CaptureAliasRecord,
  CaptureAnchorRequest,
  CaptureFidelity,
  CapturePhase,
  CaptureStubOptions,
  CaptureStubResult,
  SelfCheckOutcome,
  SerializationTier,
} from './api.js';
import { entryRelativeSpecifier, resolveAnchor, type ResolvedAnchor } from './anchors.js';
import { findAugmentationFiles } from './augmentations.js';
import { installedVersions, lockfileVersions } from './lockfile.js';
import { installedResolutionEdges, RESOLUTION_FILE } from './resolution-edges.js';
import { rewriteEmittedSpecifiers } from './paths-rewrite.js';
import { substituteMachinePathImports } from './machine-paths.js';
import { installedPackageSpecifier, typesPackageOf, withInstalledPackages } from './installed-package.js';
import { selfCheckStub, type SelfCheckArgs } from './self-check.js';
import { backfillRerun, sharingHost, type BackfillPlan } from './backfill.js';
import { collectSpecifiers, isRelative, packageNameOf } from './specifiers.js';
import { DenoProject, findDenoConfig } from './deno-project.js';
import { emitsAlike, ProjectGraph, type ServiceProject } from './project-references.js';
import { redirectingCompilerHost, referenceRedirect, type ReferenceRedirect } from './reference-redirect.js';
import { markUncarried } from './unresolved.js';
import { findServiceTsconfig } from './service-config.js';
import { placeEmittedTree, surfaceModuleInTree } from './outside-root.js';
import { WriteGuard } from './guarded-fs.js';
import { sidecarCompilerOptions } from './compiler-options.js';
import { boundedCompilerHost, boundedSys, isInsideCheckout, untrustedReadBound, type ReadBound } from './checkout.js';

export type { CaptureStubOptions, CaptureStubResult } from './api.js';
export { DenoProject, findDenoConfig } from './deno-project.js';
export { sidecarCompilerOptions } from './compiler-options.js';
export { isInsideCheckout, untrustedReadBound, UNTRUSTED_TREE_ENV, type ReadBound } from './checkout.js';
export { serviceConfigPath } from './project-references.js';
export { referenceRedirectOf } from './reference-redirect.js';
export { findServiceTsconfig, fileConfigPath } from './service-config.js';
// carrick#2405: the per-position rows write through the guard, and name an
// installed declaring file the way the capture's resolution walk does.
export { WriteGuard } from './guarded-fs.js';
export { installedPackageDirectory } from './installed-package-directory.js';
export { isDeclarationFileName, packageNameOf } from './specifiers.js';
export { moduleExport } from './anchors.js';
// v2 check core ("tsc as the judge"). Same bundle, same seam: the sidecar
// reaches it only through this door (index.js).
export { runCheck } from './check.js';
export type { CheckProgress } from './check.js';
export { jsonWireDeclarations } from './check-probe.js';
export { findDisqualifyingTopTypes } from './deep-walk.js';

const SURFACE_ENTRY_BASENAME = '__carrick_surface__';

/** Captures made by this process, so each one's entry file has a name of its
 * own. Paired with the pid it is unique across processes too. */
let surfaceEntrySequence = 0;

/**
 * The name of THIS capture's surface entry, without an extension
 * (carrick#1046).
 *
 * The entry has to live inside the effective `rootDir` — an entry beside a
 * `rootDir` of `src` fails TS6059 — so it is written into the scanned tree and
 * unlinked afterwards. One name per repo root made two captures of the same
 * tree share a single file: whichever finished first unlinked it while the
 * other's program was still reading it, and every alias whose print anchors in
 * that destination then demoted to `structural_fallback` with an accessibility
 * reason that described the harness rather than the code. Concurrent captures
 * of one tree are ordinary — the test suite does it on every run, and two
 * services of a monorepo can share a root — so the name, not the locking, is
 * what has to give.
 *
 * A leftover from an interrupted capture is also identifiable as one process's
 * (carrick#1069), rather than a fixed name the next scan reads as source.
 */
export function surfaceEntryFileName(): string {
  surfaceEntrySequence += 1;
  return `${SURFACE_ENTRY_BASENAME}.${process.pid}.${surfaceEntrySequence}`;
}

/** Same normalization intent as bundle_file_stems on the Rust side. */
export function sanitizeServiceName(name: string): string {
  return name.toLowerCase().replace(/[^a-z0-9._-]+/g, '-').replace(/^-+|-+$/g, '');
}

function fail(stubDir: string, packageName: string, errors: string[]): CaptureStubResult {
  return {
    success: false,
    stub_dir: stubDir,
    package_name: packageName,
    emitted_files: [],
    pinned_dependencies: {},
    unpinned_externals: [],
    aliases: [],
    fidelity: emptyFidelity(),
    augmentation_files: [],
    specifier_rewrites: 0,
    bare_checkout: false,
    unresolved_dependencies: [],
    ts_version: ts.version,
    errors,
  };
}

function emptyFidelity(): CaptureFidelity {
  return {
    total_aliases: 0,
    by_serialization: { emitted: 0, node_builder: 0, structural_fallback: 0 },
    by_self_check: { ok: 0, allowlisted_external: 0, decayed_internal: 0 },
    by_anchor_origin: {
      'llm-symbol': 0,
      'deterministic-infer': 0,
      'anchor-backfill': 0,
      'manifest-placeholder': 0,
    },
    usable_rate: 0,
  };
}

function computeFidelity(records: CaptureAliasRecord[]): CaptureFidelity {
  const fidelity = emptyFidelity();
  fidelity.total_aliases = records.length;
  for (const record of records) {
    fidelity.by_serialization[record.serialization as SerializationTier]++;
    fidelity.by_self_check[record.self_check as SelfCheckOutcome]++;
    fidelity.by_anchor_origin[record.anchor_origin as AnchorOrigin]++;
  }
  const usable =
    fidelity.by_self_check.ok + fidelity.by_self_check.allowlisted_external;
  fidelity.usable_rate =
    records.length === 0 ? 0 : Math.round((usable / records.length) * 1000) / 1000;
  return fidelity;
}

export function captureStub(opts: CaptureStubOptions): CaptureStubResult {
  const repoRoot = path.resolve(opts.repoRoot);
  const packageName = `@carrick/${sanitizeServiceName(opts.serviceName)}`;
  const stubDir = path.resolve(opts.outDir);
  const errors: string[] = [];
  // Under CARRICK_UNTRUSTED_TREE every compiler read below is held to the
  // checkout, the sidecar's own install and this stub; otherwise undefined
  // and nothing here changes (carrick#2230).
  // The backfilled stub (carrick#2041) is written beside the first.
  const backfillDir = plannedBackfill(opts) ? path.resolve(opts.backfillOutDir!) : undefined;
  const stubDirs = backfillDir ? [stubDir, backfillDir] : [stubDir];
  const bound = untrustedReadBound(repoRoot, stubDirs);

  // The named config, else the one the init'd project reads too
  // (carrick#1776). A service with neither is typed under defaults below.
  const configPath = opts.tsconfigPath
    ? path.resolve(repoRoot, opts.tsconfigPath)
    : findServiceTsconfig(repoRoot, opts.scanRoot) ?? path.join(repoRoot, 'tsconfig.json');

  let parsed: ts.ParsedCommandLine | undefined;
  // The config the emit's options came from: the named one, or the project
  // that owns the most anchors' files (carrick#1604).
  let projectConfigPath = configPath;
  // Anchor indexes by the project that owns their file, when the named config
  // references others and the anchors' files do not all belong to one project.
  let ownerGroups: Map<ServiceProject, number[]> | undefined;
  let emitProject: ServiceProject | undefined;
  // The projects the named config reaches, when it is a tsconfig.
  let projectGraph: ProjectGraph | undefined;
  // Everything this capture writes, it writes through `guard` (carrick#1748):
  // the stub dir, the staging dir, and the surface entry. A stub dir is
  // emptied before it is written, so one that is or holds the repo is refused
  // before anything is touched. The scan root is protected too: a stub dir
  // in a sibling service of the same repo is refused unless it sits beneath
  // a `.carrick` directory (carrick#1768).
  let guard: WriteGuard;
  try {
    const protect = opts.scanRoot === undefined ? [repoRoot] : [repoRoot, path.resolve(opts.scanRoot)];
    guard = WriteGuard.of({ dirs: stubDirs, protect });
  } catch (err) {
    return fail(stubDir, packageName, [err instanceof Error ? err.message : String(err)]);
  }
  let deno: DenoProject | undefined;
  try {
    const config = findDenoConfig(repoRoot, opts.tsconfigPath);
    if (config) deno = new DenoProject(config, repoRoot);
  } catch (err) {
    return fail(stubDir, packageName, [err instanceof Error ? err.message : String(err)]);
  }
  if (deno) {
    parsed = deno.parsed;
    errors.push(...deno.diagnostics);
  } else if (!fs.existsSync(configPath)) {
    if (opts.tsconfigPath) {
      // An explicitly named tsconfig that does not exist is a caller bug.
      return fail(stubDir, packageName, [`tsconfig not found at ${configPath}`]);
    }
    // No tsconfig in the repo: synthesize defaults (parity with the v1
    // project loader's DEFAULT_COMPILER_OPTIONS) so tsconfig-less repos
    // still capture instead of shipping no surface at all.
    parsed = ts.parseJsonConfigFileContent(
      {
        compilerOptions: {
          target: 'ESNext',
          module: 'ESNext',
          moduleResolution: 'Bundler',
          strict: true,
          esModuleInterop: true,
          skipLibCheck: true,
          allowJs: true,
          checkJs: false,
          resolveJsonModule: true,
        },
      },
      boundedSys(bound),
      repoRoot
    );
  } else {
    // Each anchor is typed under the project that owns its file: the named
    // config when it lists the file, else the first project it references
    // (depth-first, in declared order) that does, else the named config.
    let graph: ProjectGraph;
    try {
      graph = new ProjectGraph(configPath);
    } catch (err) {
      return fail(stubDir, packageName, [err instanceof Error ? err.message : String(err)]);
    }
    const groups = new Map<ServiceProject, number[]>();
    const unfiled: number[] = [];
    opts.anchors.forEach((anchor, index) => {
      const file = anchor.source_file ? path.resolve(repoRoot, anchor.source_file) : undefined;
      if (!file || !fs.existsSync(file)) {
        unfiled.push(index);
        return;
      }
      const owner = graph.ownerOf(file);
      groups.set(owner, [...(groups.get(owner) ?? []), index]);
    });
    // One emit: under the owner of the most anchors, ties to search order.
    let emit = graph.named;
    let most = 0;
    for (const [owner, indexes] of groups) {
      if (indexes.length > most || (indexes.length === most && graph.rank(owner) < graph.rank(emit))) {
        emit = owner;
        most = indexes.length;
      }
    }
    if (unfiled.length > 0) groups.set(emit, [...(groups.get(emit) ?? []), ...unfiled]);
    errors.push(...graph.diagnostics);
    projectGraph = graph;
    parsed = emit.parsed;
    projectConfigPath = emit.configPath;
    emitProject = emit;
    if (groups.size > 1) ownerGroups = groups;
  }
  if (!parsed) {
    return fail(stubDir, packageName, [`failed to parse ${configPath}`]);
  }

  // The surface entry must live inside the effective rootDir (design doc
  // Capture step 1: an entry at repo root with rootDir "src" fails TS6059).
  const entryDir = parsed.options.rootDir
    ? path.resolve(path.dirname(projectConfigPath), parsed.options.rootDir)
    : repoRoot;
  const surfaceEntry = surfaceEntryFileName();
  const surfaceDeclaration = `${surfaceEntry}.d.ts`;
  const entryPath = deno
    ? path.join(deno.cacheDir, `${surfaceEntry}.ts`)
    : path.join(entryDir, `${surfaceEntry}.ts`);
  // The entry is the one file a capture writes inside the repo itself: tsc
  // only emits it from inside rootDir. It is written, read and deleted, and
  // the guard holds it to exactly that path. A Deno entry sits in the cache.
  try {
    guard = guard.with(deno ? { dirs: [deno.cacheDir] } : { files: [entryPath] });
    guard.mkdir(path.dirname(entryPath));
  } catch (err) {
    return fail(stubDir, packageName, [err instanceof Error ? err.message : String(err)]);
  }

  // ---- Phase A: analysis program over placeholder entry + anchor sources ----
  let analysis: Analysis | undefined;
  const progress = opts.onProgress ?? (() => {});
  // The files of a project that declare a module or global augmentation are
  // roots of both programs below: the one its anchors are read in and the one
  // that emits the surface (carrick#2317). Read once per project.
  const augmentations = augmentationReader(bound);
  // Each anchor program reads the projects its owner references as an editor
  // does (carrick#2418). The emit below keeps the compiler's own resolution.
  const graphForRedirect = projectGraph;
  const redirectFor = (owner: ServiceProject): ReferenceRedirect | undefined =>
    graphForRedirect ? referenceRedirect(graphForRedirect.closureOf(owner), repoRoot) : undefined;
  const analysisCtx: AnalysisContext = {
    repoRoot,
    entryDir: path.dirname(entryPath),
    entryPath,
    guard,
    progress,
    bound,
    augmentations,
    redirectFor,
    readsReferences: graphForRedirect?.hasReferences ?? false,
  };
  try {
    analysis = ownerGroups && emitProject
      ? resolveAnchorsByOwner(opts, ownerGroups, emitProject, analysisCtx)
      : resolveAnchors(opts, parsed, analysisCtx, deno, emitProject);
  } catch (err) {
    return fail(stubDir, packageName, [err instanceof Error ? err.message : String(err)]);
  }

  // ---- Augmentation files of the project that emits the surface ----
  const augmentationSources = [...new Set([...augmentations(parsed), ...(deno?.globals ?? [])])];

  // ---- Phase B: declaration emit of the final entry ----
  const scratch = WriteGuard.scratch('carrick-capture-v2-');
  const staging = scratch.dir;
  const emitOptions: ts.CompilerOptions = sidecarCompilerOptions({
    ...parsed.options,
    // The load-bearing trio: emit declarations without checking, so
    // type-error-laden and bare (no node_modules) checkouts still emit.
    noCheck: true,
    declaration: true,
    emitDeclarationOnly: true,
    noEmit: false,
    declarationMap: false,
    composite: false,
    incremental: false,
    outDir: staging,
    rootDir: entryDir,
  });
  const emitCtx: EmitContext = {
    entryPath,
    entryDir,
    surfaceDeclaration,
    guard,
    progress,
    rootNames: [entryPath, ...augmentationSources],
    options: emitOptions,
    // A backfill's emit differs from the first only in the entry: every other
    // file it reads is answered with the one the first emit parsed.
    host: plannedBackfill(opts)
      ? sharingHost(programHost(emitOptions, deno, bound) ?? ts.createCompilerHost(emitOptions), (file) => path.resolve(file) === entryPath)
      : programHost(emitOptions, deno, bound),
    staging,
  };
  let first: SurfaceEmit;
  const analysisErrors = analysis.errors;
  let backfill: { plan: BackfillPlan; surface: SurfaceEmit; errors: string[] } | undefined;
  try {
    try {
      first = emitSurface(analysis.resolved, emitCtx);
    } catch (err) {
      return fail(stubDir, packageName, [err instanceof Error ? err.message : String(err)]);
    }
    // ---- Literal backfill (carrick#2041): the demoted aliases the scanner
    // holds a text for, re-anchored on it, read in the same anchor program
    // and emitted on the files the first emit parsed.
    const plan = plannedBackfill(opts) ? backfillRerun(opts.anchors, first.resolved, opts.backfillTexts) : undefined;
    if (plan) {
      try {
        const again = analysis.resolveAgain(plan.anchors, new Set(plan.backfilled));
        progress('emit', 'emitting the backfilled surface');
        backfill = { plan, surface: emitSurface(again.resolved, emitCtx), errors: again.errors };
      } catch (err) {
        console.error(`[sidecar] capture ${packageName}: backfill surface not written: ${err instanceof Error ? err.message : String(err)}`);
      }
    }
  } finally {
    // The anchor programs, and the files the emits parsed, are held only
    // until the backfill has read them.
    analysis = undefined;
    emitCtx.host = undefined;
    scratch.guard.remove(staging);
  }

  // ---- What the checkout installs (shared by both stubs) ----
  const dependencyRoot = deno?.config.workspaceRoot ?? repoRoot;
  const nodeModulesPresent = fs.existsSync(path.join(dependencyRoot, 'node_modules'));
  const declared =
    deno || !nodeModulesPresent
      ? { unresolved: [], runtimeDeclared: 0, runtimeResolved: 0 }
      : declaredDependencyResolution(dependencyRoot);
  const unresolvedDependencies = declared.unresolved;
  // Bare: no node_modules, or every declared runtime dependency dangles (a
  // missing pnpm store). One missing package, or missing devDependencies,
  // leaves the installed ones usable (carrick#2999).
  const bareCheckout =
    !deno &&
    (!nodeModulesPresent || (declared.runtimeDeclared > 0 && declared.runtimeResolved === 0));
  if (unresolvedDependencies.length > 0) {
    console.error(
      `[sidecar] capture ${packageName}: declared dependencies do not resolve` +
        `${bareCheckout ? ' (bare checkout)' : ''}: ${unresolvedDependencies.join(', ')}`
    );
  }

  const stubCtx: StubContext = {
    guard,
    packageName,
    staging,
    entryDir,
    surfaceDeclaration,
    augmentationSources,
    parsed,
    projectConfigPath,
    deno,
    repoRoot,
    labelRoot: opts.scanRoot ?? repoRoot,
  };
  let stub: WrittenStub;
  try {
    stub = writeStub(first, stubDir, stubCtx);
  } catch (err) {
    if (err instanceof StubFailure) return fail(stubDir, packageName, [err.message]);
    throw err;
  }
  let second: WrittenStub | undefined;
  if (backfill) {
    try {
      second = writeStub(backfill.surface, backfillDir!, stubCtx);
    } catch (err) {
      console.error(`[sidecar] capture ${packageName}: backfill stub not written: ${err instanceof Error ? err.message : String(err)}`);
      guard.remove(backfillDir!);
    }
  }

  // ---- Capture-time self-check (per-alias closure attribution) ----
  progress('self-check', 'checking the stub');
  const hostFor = (installs: Record<string, string>) =>
    deno || bound || Object.keys(installs).length > 0
      ? (options: ts.CompilerOptions) =>
          boundedCompilerHost(
            withInstalledPackages(options, installs, deno ? deno.host(options) : undefined),
            bound
          )
      : undefined;
  const checkArgs = (written: WrittenStub, compilerHost: SelfCheckArgs['compilerHost']): SelfCheckArgs => ({
    guard: written.guard,
    stubDir: written.stubDir,
    surfaces: [{ surfaceAbsPath: written.surfaceAbsPath, resolved: written.resolved }],
    pinned: written.pinned,
    bareCheckout,
    repoRoot: dependencyRoot,
    compilerHost,
  });
  let aliases: CaptureAliasRecord[];
  let secondAliases: CaptureAliasRecord[] | undefined;
  if (!second) {
    [aliases] = selfCheckStub(checkArgs(stub, hostFor(stub.installs)));
  } else {
    ({ first: aliases, second: secondAliases } = selfCheckBoth(stub, second, checkArgs, hostFor));
  }

  const result = finishStub(stub, aliases, {
    packageName,
    bareCheckout,
    unresolvedDependencies,
    errors: [...errors, ...analysisErrors, ...first.errors],
  });
  if (!second || !secondAliases || !backfill) {
    if (second) guard.remove(second.stubDir);
    return result;
  }
  return {
    ...result,
    backfill: {
      backfilled: backfill.plan.backfilled,
      result: finishStub(second, secondAliases, {
        packageName,
        bareCheckout,
        unresolvedDependencies,
        errors: [...errors, ...backfill.errors, ...backfill.surface.errors],
      }),
    },
  };
}

/** Whether this capture may write a backfilled second stub (carrick#2041). */
function plannedBackfill(opts: CaptureStubOptions): boolean {
  return (
    opts.backfillOutDir !== undefined &&
    opts.backfillTexts !== undefined &&
    Object.keys(opts.backfillTexts).length > 0
  );
}

/** What Phase B reads beside the resolved anchors: one per capture. */
interface EmitContext {
  entryPath: string;
  entryDir: string;
  /** The emitted name of this capture's surface entry (carrick#1046). */
  surfaceDeclaration: string;
  guard: WriteGuard;
  progress: (phase: CapturePhase, message: string) => void;
  rootNames: string[];
  options: ts.CompilerOptions;
  /** Released (set undefined) once every surface is emitted. */
  host: ts.CompilerHost | undefined;
  staging: string;
}

/** One surface's declaration emit, after the partial-emit demotion. */
interface SurfaceEmit extends Omit<DeclarationEmit, 'partial' | 'diagnostics'> {
  resolved: ResolvedAnchor[];
  /** The emit's diagnostics, then the partial-emit note when there is one. */
  errors: string[];
}

/**
 * Phase B for one surface: write the entry that aliases each resolved anchor,
 * emit its declarations, and demote the aliases a partial emit leaves
 * dangling. Throws when the emit wrote nothing.
 */
function emitSurface(resolved: ResolvedAnchor[], ctx: EmitContext): SurfaceEmit {
  const entryLines = ['// Generated by Carrick capture v2. Deleted after emit.'];
  for (const anchor of resolved) {
    const comment = anchor.failureReason
      ? ` // capture-degraded: ${anchor.failureReason.replace(/\n/g, ' ')}`
      : '';
    entryLines.push(
      `export type ${anchor.request.alias} = ${anchor.aliasText};${comment}`
    );
  }

  let written: DeclarationEmit;
  try {
    ctx.progress('emit', 'emitting declarations');
    ctx.guard.writeFile(ctx.entryPath, entryLines.join('\n') + '\n');
    written = emitDeclarations({
      rootNames: ctx.rootNames,
      options: ctx.options,
      host: ctx.host,
      staging: ctx.staging,
      entryDir: ctx.entryDir,
    });
  } finally {
    if (fs.existsSync(ctx.entryPath)) ctx.guard.unlink(ctx.entryPath);
  }
  const { emitted, declarationSources, sourceByEmitted } = written;
  const errors = [...written.diagnostics];

  // ---- Partial-emit recovery ----
  // The corpus-2 notifications-svc shape: one file's declaration emit was
  // skipped but the rest of the tree emitted fine. Keep the tree; demote any
  // alias whose surface reference would dangle (its module produced no .d.ts)
  // and rewrite its surface line to `unknown` so the kept tree carries no
  // dangling specifiers that would smear the healthy aliases at self-check or
  // poison the whole service at check time. Fail-closed: a demoted alias is
  // `unknown` at the surface, so the check phase's IsUnknown probe gate (or
  // the poison rule, for unemitted modules still referenced by kept files)
  // decays it to unverifiable — it can never read compatible.
  if (written.partial) {
    errors.push(
      `declaration emit was partial: kept ${emitted.size} emitted file(s); ` +
        'aliases referencing unemitted modules are demoted to structural_fallback'
    );
    resolved = demoteDanglingAliases({
      resolved,
      emitted,
      declarationSources,
      staging: ctx.staging,
      entryDir: ctx.entryDir,
      surfaceDeclaration: ctx.surfaceDeclaration,
    });
  }
  return { resolved, emitted, declarationSources, sourceByEmitted, errors };
}

/** What writing a stub package reads beside its surface: one per capture. */
interface StubContext {
  guard: WriteGuard;
  packageName: string;
  staging: string;
  entryDir: string;
  surfaceDeclaration: string;
  augmentationSources: string[];
  parsed: ts.ParsedCommandLine;
  projectConfigPath: string;
  deno: DenoProject | undefined;
  repoRoot: string;
  /** The root machine paths are named from (carrick#2943). */
  labelRoot: string;
}

/** A stub package written up to its self-check. */
interface WrittenStub {
  stubDir: string;
  /** Writes held to this stub. */
  guard: WriteGuard;
  surfaceAbsPath: string;
  /** The surface's anchors, with what the machine-path pass found. */
  resolved: ResolvedAnchor[];
  /** Paths relative to `types/`. */
  emittedFiles: string[];
  augmentationFiles: string[];
  specifierRewrites: number;
  pinned: Record<string, string>;
  unpinned: string[];
  /** Package -> installed directory, for specifiers rewritten into one. */
  installs: Record<string, string>;
}

/** A stub that cannot be written: the capture fails with this message alone. */
class StubFailure extends Error {}

/**
 * Write one surface's stub package into `stubDir`: the emitted tree with its
 * specifiers rewritten, the pins, the recorded dependency edges,
 * `package.json` and the tsconfig snapshot. The self-check and the manifest
 * follow.
 */
function writeStub(surface: SurfaceEmit, stubDir: string, ctx: StubContext): WrittenStub {
  const { emitted, declarationSources, sourceByEmitted } = surface;
  let resolved = surface.resolved;
  const { guard, deno, parsed } = ctx;

  // ---- Relocate the emitted tree into the stub package ----
  const typesDir = path.join(stubDir, 'types');
  guard.remove(stubDir);
  guard.mkdir(typesDir);
  // From here on, only the stub is written.
  const stubGuard = guard.narrow(stubDir);

  // A declaration for a source outside rootDir arrives at the source's own
  // path; it is placed under the tree too (carrick#1770).
  const placed = placeEmittedTree({
    emitted,
    staging: ctx.staging,
    entryDir: ctx.entryDir,
    surfaceDeclaration: ctx.surfaceDeclaration,
  });
  const emittedFiles: string[] = [];
  let surfaceAbsPath = '';
  for (const fileName of emitted.keys()) {
    const stagingRel = path.relative(ctx.staging, fileName).split(path.sep).join('/');
    const rel = placed.relOf.get(fileName)!;
    const text = placed.textOf.get(fileName)!;
    if (rel !== stagingRel) {
      const source = sourceByEmitted.get(stagingRel);
      sourceByEmitted.delete(stagingRel);
      if (source) sourceByEmitted.set(rel, source);
    }
    const dest = path.join(typesDir, rel);
    stubGuard.mkdir(path.dirname(dest));
    stubGuard.writeFile(dest, text);
    emittedFiles.push(rel);
    if (rel === 'surface.d.ts') surfaceAbsPath = dest;
  }
  if (!surfaceAbsPath) {
    throw new StubFailure('no surface.d.ts produced by emit');
  }
  // Verbatim copies of in-repo declaration sources (see declarationSources).
  for (const [rel, text] of declarationSources) {
    if (emittedFiles.includes(rel)) continue;
    const dest = path.join(typesDir, rel);
    stubGuard.mkdir(path.dirname(dest));
    stubGuard.writeFile(dest, text);
    emittedFiles.push(rel);
  }

  // Tree-relative names of augmentation files that made it into the tree
  // (.ts augmentations arrive via declaration emit, .d.ts ones verbatim).
  const augmentationFiles = ctx.augmentationSources
    .map((abs) => {
      const noDts = abs.replace(/\.d\.ts$/, '');
      const noExt = noDts === abs ? abs.replace(/\.(ts|tsx|mts|cts)$/, '') : noDts;
      const outside = placed.outside.get(path.resolve(noExt));
      if (outside !== undefined) return outside;
      const rel = path.relative(ctx.entryDir, noExt).split(path.sep).join('/');
      return `${rel}.d.ts`;
    })
    .filter((rel) => emittedFiles.includes(rel))
    .map((rel) => `types/${rel}`);

  // ---- Post-emit specifier rewrite (paths mappings + absolute internals) ----
  let denoRewrites = 0;
  try {
    denoRewrites = deno?.rewrite(stubGuard, typesDir, emittedFiles, sourceByEmitted) ?? 0;
  } catch (err) {
    throw new StubFailure(err instanceof Error ? err.message : String(err));
  }
  const rewritten = rewriteEmittedSpecifiers({
    guard: stubGuard,
    outside: placed.outside,
    typesDir,
    files: emittedFiles,
    options: parsed.options,
    configPath: ctx.projectConfigPath,
    entryDir: ctx.entryDir,
  });
  const specifierRewrites = denoRewrites + placed.rewrites + rewritten.rewrites;
  // What the rewrite could not map still names a machine path (carrick#2943).
  const machinePaths = substituteMachinePathImports({
    files: emittedFiles.map((rel) => path.join(typesDir, rel)),
    surfaceAbsPath,
    tree: stubGuard.narrow(typesDir),
    labelRoot: ctx.labelRoot,
  });
  if (machinePaths.size > 0) {
    resolved = resolved.map((anchor) => {
      const found = machinePaths.get(anchor.request.alias);
      if (!found) return anchor;
      const paths = [...new Set([...(anchor.unresolved?.paths ?? []), ...found.paths])];
      const specifiers = [...new Set([...found.modules, ...(anchor.unresolved?.specifiers ?? [])])];
      return { ...anchor, unresolved: { paths, specifiers } };
    });
  }

  // ---- Pin external deps: installed node_modules first, lockfile fallback ----
  // Externals are collected AFTER the rewrite pass: a rewritten paths
  // specifier is internal, not a dependency.
  const externalSpecs = new Set<string>();
  for (const rel of emittedFiles) {
    const text = fs.readFileSync(path.join(typesDir, rel), 'utf8');
    for (const spec of collectSpecifiers(text)) {
      if (!isRelative(spec) && !spec.startsWith('node:')) {
        externalSpecs.add(packageNameOf(spec));
      }
    }
  }
  // Precedence: an installed checkout's node_modules is what the repo
  // actually resolves against, so it wins over the lockfile — and it pins
  // repos whose lockfiles we do not parse (yarn classic v1, binary
  // bun.lockb). The parsed lockfile (npm, pnpm, yarn-berry, text bun.lock)
  // remains the bare-checkout fallback. Both paths pin only
  // the directly-referenced externals; what those depend on is recorded
  // from the installed tree below (RESOLUTION_FILE).
  const installed = installedVersions(ctx.repoRoot, externalSpecs);
  const lockVersions = lockfileVersions(ctx.repoRoot);
  for (const name of Object.keys(deno?.pinned ?? {})) externalSpecs.add(name);
  // A package an absolute specifier was rewritten into carries the version
  // installed at that path (#1174); it may be a transitive the repo root
  // neither installs by name nor locks.
  for (const name of Object.keys(rewritten.pins)) externalSpecs.add(name);
  const pinned: Record<string, string> = {};
  const unpinned: string[] = [];
  for (const name of [...externalSpecs].sort()) {
    const version =
      deno?.pinned[name] ?? rewritten.pins[name] ?? installed.get(name) ?? lockVersions.get(name);
    if (version) pinned[name] = version;
    // A runtime name whose declarations come from a rewritten `@types/*`
    // package resolves through that pin.
    else if (!rewritten.pins[typesPackageOf(name)]) unpinned.push(name);
  }

  // ---- Record how the installed tree resolved what the pins depend on ----
  // The check pins each recorded edge, so it installs what the repo
  // installed rather than the newest version in range (carrick#2091).
  const resolution = deno
    ? { edges: {}, unrecorded: 0 }
    : installedResolutionEdges(ctx.repoRoot, pinned);
  const edgeCount = Object.values(resolution.edges).reduce(
    (n, children) => n + Object.keys(children).length,
    0
  );
  if (edgeCount > 0) {
    stubGuard.writeFile(
      path.join(stubDir, RESOLUTION_FILE),
      JSON.stringify({ edges: resolution.edges }, null, 2) + '\n'
    );
  }
  console.error(
    `[sidecar] capture ${ctx.packageName}: ${edgeCount} dependency edge(s) recorded from the installed tree, ` +
      `${resolution.unrecorded} left to the registry`
  );

  stubGuard.writeFile(
    path.join(stubDir, 'package.json'),
    JSON.stringify(
      {
        name: ctx.packageName,
        version: '0.0.0-carrick',
        private: true,
        types: './types/surface.d.ts',
        dependencies: pinned,
      },
      null,
      2
    ) + '\n'
  );
  stubGuard.writeFile(
    path.join(stubDir, 'tsconfig.snapshot.json'),
    JSON.stringify(
      {
        ts_version: ts.version,
        strict: parsed.options.strict ?? false,
        strictNullChecks: parsed.options.strictNullChecks ?? parsed.options.strict ?? false,
        exactOptionalPropertyTypes: parsed.options.exactOptionalPropertyTypes ?? false,
        module:
          parsed.options.module !== undefined ? ts.ModuleKind[parsed.options.module] : undefined,
        target:
          parsed.options.target !== undefined ? ts.ScriptTarget[parsed.options.target] : undefined,
      },
      null,
      2
    ) + '\n'
  );

  return {
    stubDir,
    guard: stubGuard,
    surfaceAbsPath,
    resolved,
    emittedFiles,
    augmentationFiles,
    specifierRewrites,
    pinned,
    unpinned,
    installs: rewritten.installs,
  };
}

/**
 * The name the backfilled surface is checked under, beside the first, when
 * both stubs hold the same tree. No emitted declaration is named so: an
 * emitted name keeps its source's extension stem, and this one has two dots
 * no source file name would end with.
 */
const JOINT_SURFACE = 'surface.carrick-backfill.d.ts';

/**
 * Self-check both stubs of a backfill (carrick#2041).
 *
 * When the two stubs hold the same tree beside their surfaces, both surfaces
 * are checked in one program over the first stub's tree, which is what the
 * check costs: every declaration of the tree and of the installed packages it
 * reaches. Each surface is a module of its own and nothing imports either, so
 * neither changes what the other's aliases read, and each alias's closure is
 * walked from its own surface. What the repair pass rewrote in the shared
 * tree is copied into the second stub, which holds the same files.
 *
 * Otherwise (the backfilled aliases were the only route to some module, so
 * the trees differ) each stub is checked in a program of its own, which is
 * what the stub would be checked in alone; the two programs share the files
 * they parse outside the stubs.
 */
function selfCheckBoth(
  first: WrittenStub,
  second: WrittenStub,
  checkArgs: (written: WrittenStub, compilerHost: SelfCheckArgs['compilerHost']) => SelfCheckArgs,
  hostFor: (installs: Record<string, string>) => SelfCheckArgs['compilerHost']
): { first: CaptureAliasRecord[]; second: CaptureAliasRecord[] | undefined } {
  const typesOf = (written: WrittenStub) => path.join(written.stubDir, 'types');
  if (sameTreeBesideSurface(first, second)) {
    const joint = path.join(typesOf(first), JOINT_SURFACE);
    first.guard.writeFile(joint, fs.readFileSync(second.surfaceAbsPath, 'utf8'));
    let records: CaptureAliasRecord[][];
    try {
      const installs = { ...second.installs, ...first.installs };
      records = selfCheckStub({
        ...checkArgs(first, hostFor(installs)),
        surfaces: [
          { surfaceAbsPath: first.surfaceAbsPath, resolved: first.resolved },
          { surfaceAbsPath: joint, resolved: second.resolved },
        ],
        pinned: { ...second.pinned, ...first.pinned },
      });
    } catch (err) {
      // The first stub is checked alone, as it would be with no backfill.
      console.error(`[sidecar] backfill self-check failed, first stub checked alone: ${err instanceof Error ? err.message : String(err)}`);
      if (fs.existsSync(joint)) first.guard.unlink(joint);
      return { first: selfCheckStub(checkArgs(first, hostFor(first.installs)))[0], second: undefined };
    } finally {
      if (fs.existsSync(joint)) first.guard.unlink(joint);
    }
    for (const rel of second.emittedFiles) {
      if (rel === 'surface.d.ts') continue;
      const text = fs.readFileSync(path.join(typesOf(first), rel), 'utf8');
      const dest = path.join(typesOf(second), rel);
      if (fs.readFileSync(dest, 'utf8') !== text) second.guard.writeFile(dest, text);
    }
    return { first: records[0], second: records[1] };
  }

  const inStubs = (file: string) => {
    const abs = path.resolve(file);
    return [first.stubDir, second.stubDir].some((dir) => abs === dir || abs.startsWith(dir + path.sep));
  };
  const parsedOnce = new Map<string, ts.SourceFile | undefined>();
  const sharing = (installs: Record<string, string>): SelfCheckArgs['compilerHost'] => {
    const host = hostFor(installs);
    return (options) => sharingHost(host?.(options) ?? ts.createCompilerHost(options), inStubs, parsedOnce);
  };
  const [firstRecords] = selfCheckStub(checkArgs(first, sharing(first.installs)));
  let secondRecords: CaptureAliasRecord[] | undefined;
  try {
    [secondRecords] = selfCheckStub(checkArgs(second, sharing(second.installs)));
  } catch (err) {
    console.error(`[sidecar] backfill self-check failed: ${err instanceof Error ? err.message : String(err)}`);
  }
  return { first: firstRecords, second: secondRecords };
}

/** Whether two stubs hold the same files, byte for byte, beside `surface.d.ts`. */
function sameTreeBesideSurface(first: WrittenStub, second: WrittenStub): boolean {
  const besides = (written: WrittenStub) => written.emittedFiles.filter((rel) => rel !== 'surface.d.ts').sort();
  const a = besides(first);
  const b = besides(second);
  if (a.length !== b.length || a.some((rel, i) => rel !== b[i])) return false;
  if (a.includes(JOINT_SURFACE)) return false;
  return a.every(
    (rel) =>
      fs.readFileSync(path.join(first.stubDir, 'types', rel), 'utf8') ===
      fs.readFileSync(path.join(second.stubDir, 'types', rel), 'utf8')
  );
}

/** Write a stub's manifest and answer it. */
function finishStub(
  written: WrittenStub,
  aliases: CaptureAliasRecord[],
  args: { packageName: string; bareCheckout: boolean; unresolvedDependencies: string[]; errors: string[] }
): CaptureStubResult {
  const fidelity = computeFidelity(aliases);
  written.guard.writeFile(
    path.join(written.stubDir, 'carrick-manifest.json'),
    JSON.stringify(
      {
        package_name: args.packageName,
        ts_version: ts.version,
        bare_checkout: args.bareCheckout,
        aliases,
        fidelity,
      },
      null,
      2
    ) + '\n'
  );

  return {
    success: true,
    stub_dir: written.stubDir,
    package_name: args.packageName,
    emitted_files: written.emittedFiles.map((rel) => `types/${rel}`).sort(),
    pinned_dependencies: written.pinned,
    unpinned_externals: written.unpinned,
    aliases,
    fidelity,
    augmentation_files: written.augmentationFiles.sort(),
    specifier_rewrites: written.specifierRewrites,
    bare_checkout: args.bareCheckout,
    unresolved_dependencies: args.unresolvedDependencies,
    ts_version: ts.version,
    errors: args.errors,
  };
}

/** How many unresolved packages a capture result names. */
const UNRESOLVED_DEPENDENCIES_NAMED = 5;

/**
 * How `root/package.json` dependencies resolve from `root`: no
 * `node_modules/<name>` in `root` or any parent directory means unresolved.
 * `existsSync` follows links, so a pnpm link into a missing store reads as
 * unresolved (carrick#2999). `unresolved` is the first few names across
 * dependencies and devDependencies, sorted; the runtime counts cover
 * `dependencies` only. Declared names only, one stat per ancestor.
 */
function declaredDependencyResolution(root: string): {
  unresolved: string[];
  runtimeDeclared: number;
  runtimeResolved: number;
} {
  const none = { unresolved: [], runtimeDeclared: 0, runtimeResolved: 0 };
  let manifest: unknown;
  try {
    manifest = JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8'));
  } catch {
    return none;
  }
  if (manifest === null || typeof manifest !== 'object') return none;
  const names = (field: string): string[] => {
    const group = (manifest as Record<string, unknown>)[field];
    return group !== null && typeof group === 'object' ? Object.keys(group) : [];
  };
  const resolves = (name: string): boolean => {
    let dir = path.resolve(root);
    for (;;) {
      if (fs.existsSync(path.join(dir, 'node_modules', name))) return true;
      const parent = path.dirname(dir);
      if (parent === dir) return false;
      dir = parent;
    }
  };
  const runtime = new Set(names('dependencies'));
  const all = new Set([...runtime, ...names('devDependencies')]);
  let runtimeResolved = 0;
  const unresolved: string[] = [];
  for (const name of [...all].sort()) {
    if (resolves(name)) {
      if (runtime.has(name)) runtimeResolved++;
    } else {
      unresolved.push(name);
    }
  }
  return {
    unresolved: unresolved.slice(0, UNRESOLVED_DEPENDENCIES_NAMED),
    runtimeDeclared: runtime.size,
    runtimeResolved,
  };
}

/** What one, as text: nothing here is a compiler object. */
interface DeclarationEmit {
  /** Staging-absolute emitted file -> its text. */
  emitted: Map<string, string>;
  /**
   * Input .d.ts files (ambient stubs, augmentation declarations, local
   * hand-written declarations in the import closure), by their path from
   * `entryDir`. They are never re-emitted by tsc; they must ship verbatim or
   * the tree's references to them dangle.
   */
  declarationSources: Map<string, string>;
  /** Tree-relative file -> the source file it was emitted or copied from. */
  sourceByEmitted: Map<string, string>;
  /** The compiler skipped at least one file's declaration and wrote the rest. */
  partial: boolean;
  /** The emit's own diagnostics, as text. */
  diagnostics: string[];
}

/**
 * Phase B: build the program of the final entry and emit its declarations.
 *
 * A function of its own so that its program dies with it (carrick#1916). The
 * program is as large as the service, and nothing after the emit reads it:
 * what the capture needs is the text returned here. While the emit ran inside
 * `captureStub`, that one long function's frame went on holding the program
 * for as long as the capture ran, so the self-check loaded its own program
 * beside it; on a 2,345-file service that was 1,290 MB of a 2,741 MB peak.
 *
 * The caller builds the options and the host. A host is held by its program
 * and holds none itself, so the caller keeping one keeps no program alive.
 *
 * Throws when the emit was skipped and wrote nothing.
 */
function emitDeclarations(args: {
  rootNames: string[];
  options: ts.CompilerOptions;
  host: ts.CompilerHost | undefined;
  /** The directory the emit is written under (`options.outDir`). */
  staging: string;
  /** The directory the entry sits in (`options.rootDir`). */
  entryDir: string;
}): DeclarationEmit {
  const emitted = new Map<string, string>();
  const declarationSources = new Map<string, string>();
  const sourceByEmitted = new Map<string, string>();
  const program = ts.createProgram(args.rootNames, sidecarCompilerOptions(args.options), args.host);
  const emitResult = program.emit(
    undefined,
    (fileName, text, _bom, _error, sources) => {
      emitted.set(fileName, text);
      if (sources?.[0]) sourceByEmitted.set(path.relative(args.staging, fileName).split(path.sep).join('/'), sources[0].fileName);
    },
    undefined,
    /* emitOnlyDtsFiles */ true
  );
  // emitSkipped is PER-PROGRAM even when only one file's declaration emit
  // failed (e.g. TS4023 from a hand-rolled ambient stub shadowing a real
  // package): every other file's .d.ts was still written to the callback.
  // Fail wholesale only when nothing at all emitted; otherwise keep the
  // emitted subset and demote exactly the aliases it cannot support.
  if (emitResult.emitSkipped && emitted.size === 0) {
    throw new Error('declaration emit was skipped');
  }
  for (const sourceFile of program.getSourceFiles()) {
    if (!sourceFile.isDeclarationFile) continue;
    const abs = path.resolve(sourceFile.fileName);
    const rel = path.relative(args.entryDir, abs).split(path.sep).join('/');
    if (rel.startsWith('..') || rel.includes('node_modules/')) continue;
    declarationSources.set(rel, sourceFile.getFullText());
    sourceByEmitted.set(rel, abs);
  }
  return {
    emitted,
    declarationSources,
    sourceByEmitted,
    partial: emitResult.emitSkipped,
    diagnostics: emitResult.diagnostics.map((d) => ts.flattenDiagnosticMessageText(d.messageText, '\n')),
  };
}

/**
 * Partial-emit demotion: demote every anchor whose alias text names, by a
 * relative or an absolute path, a module the stub will not resolve, and
 * rewrite the demoted aliases' lines in the emitted surface to `unknown`.
 *
 * A path resolves when the tree holds its module (`surfaceModuleInTree`: an
 * emitted declaration inside rootDir or outside it, or a verbatim declaration
 * source), read from where the entry was written, not from where the surface
 * sits in the tree. An absolute path into an installed package resolves too
 * (carrick#1773): no emit writes that module, and the specifier rewrite turns
 * the path into the package's bare specifier and a pin, as it does when the
 * emit is whole. A relative path into a package is not rewritten, so it stays
 * demoted (carrick#1857).
 *
 * Anchors already demoted stay as they are; anchors whose text is
 * self-contained (node-builder structural prints, literal object text) are
 * untouched even when their source file failed to emit — their surface line
 * references nothing that can dangle.
 */
function demoteDanglingAliases(args: {
  resolved: ResolvedAnchor[];
  /** Staging-absolute emitted file -> text. Surface text is patched in place. */
  emitted: Map<string, string>;
  /** entryDir-relative verbatim .d.ts sources that will ship with the tree. */
  declarationSources: Map<string, string>;
  staging: string;
  /** The emit's rootDir: the tree mirrors it. */
  entryDir: string;
  /** The emitted name of this capture's surface entry (carrick#1046). */
  surfaceDeclaration: string;
}): ResolvedAnchor[] {
  const surfaceKey = [...args.emitted.keys()].find(
    (fileName) => path.basename(fileName) === args.surfaceDeclaration
  );
  const inTree = surfaceModuleInTree({
    emitted: args.emitted.keys(),
    declarationSources: args.declarationSources.keys(),
    staging: args.staging,
    entryDir: args.entryDir,
    surfaceDeclaration: args.surfaceDeclaration,
  });
  const resolves = new Map<string, boolean>();
  const moduleResolves = (spec: string): boolean => {
    let answer = resolves.get(spec);
    if (answer === undefined) {
      answer = inTree(spec) || (spec.startsWith('/') && installedPackageSpecifier(spec) !== undefined);
      resolves.set(spec, answer);
    }
    return answer;
  };

  const demoted = new Set<string>();
  const next = args.resolved.map((anchor): ResolvedAnchor => {
    if (anchor.failureReason !== undefined) return anchor;
    const dangling = [...collectSpecifiers(anchor.aliasText)].find(
      (spec) => isRelative(spec) && !moduleResolves(spec)
    );
    if (dangling === undefined) return anchor;
    demoted.add(anchor.request.alias);
    return {
      request: anchor.request,
      aliasText: 'unknown',
      serialization: 'structural_fallback',
      failureReason:
        `declaration emit was skipped for module '${dangling}'; ` +
        'alias demoted to keep the partially emitted tree usable',
      namesUnemittedModule: true,
    };
  });

  if (surfaceKey !== undefined && demoted.size > 0) {
    args.emitted.set(
      surfaceKey,
      rewriteSurfaceAliasesToUnknown(args.emitted.get(surfaceKey)!, demoted)
    );
  }
  return next;
}

/** Replace `export type <name> = ...;` with `= unknown` for each demoted
 * alias in the emitted surface text (span-accurate, statement-level). */
function rewriteSurfaceAliasesToUnknown(text: string, demoted: Set<string>): string {
  const sf = ts.createSourceFile('surface.d.ts', text, ts.ScriptTarget.Latest, true);
  const spans: Array<{ start: number; end: number; name: string }> = [];
  for (const stmt of sf.statements) {
    if (ts.isTypeAliasDeclaration(stmt) && demoted.has(stmt.name.text)) {
      spans.push({ start: stmt.getStart(sf), end: stmt.getEnd(), name: stmt.name.text });
    }
  }
  let out = text;
  for (const span of spans.reverse()) {
    out =
      out.slice(0, span.start) +
      `export type ${span.name} = unknown;` +
      out.slice(span.end);
  }
  return out;
}

/**
 * Phase A when the anchors' files belong to different projects (carrick#1604):
 * each owner's anchors are resolved in a program built from that owner's
 * options, so a file is never typed under a project that does not own it.
 *
 * The surface is emitted once, under `emit`'s options. An anchor from another
 * project keeps its text when the text stands alone, or when the two projects
 * would emit a declaration the same way. Otherwise its text names a module the
 * emit would declare under the wrong options (a symbol or handler anchor, or
 * a print that imports a module), so it is demoted with the reason, and cannot
 * self-check clean.
 */
function resolveAnchorsByOwner(
  opts: CaptureStubOptions,
  groups: Map<ServiceProject, number[]>,
  emit: ServiceProject,
  ctx: AnalysisContext
): Analysis {
  const analyses = new Map<ServiceProject, Analysis>();
  for (const [owner, indexes] of groups) {
    const anchors = indexes.map((index) => opts.anchors[index]);
    analyses.set(owner, resolveAnchors({ ...opts, anchors }, owner.parsed, ctx, undefined, owner));
  }
  // Each owner's anchors as its program read them, under the one emit.
  const underEmit = (read: (owner: ServiceProject, analysis: Analysis, indexes: number[]) => ResolvedAnchor[]) => {
    const resolved = new Array<ResolvedAnchor>(opts.anchors.length);
    const errors: string[] = [];
    for (const [owner, indexes] of groups) {
      let demoted = 0;
      read(owner, analyses.get(owner)!, indexes).forEach((anchor, position) => {
        const index = indexes[position];
        const namesModule =
          anchor.request.kind === 'symbol' ||
          anchor.request.kind === 'handler_return' ||
          collectSpecifiers(anchor.aliasText).size > 0;
        if (owner === emit || anchor.failureReason !== undefined || !namesModule || emitsAlike(owner, emit)) {
          resolved[index] = anchor;
          return;
        }
        demoted += 1;
        resolved[index] = {
          request: anchor.request,
          aliasText: 'unknown',
          serialization: 'structural_fallback',
          failureReason:
            `typed under ${owner.configPath}, the project that owns its file, whose options differ ` +
            `from ${emit.configPath}, which emits the surface; the module it names would be declared under the wrong options`,
        };
      });
      if (owner !== emit) {
        errors.push(
          `${indexes.length} anchor(s) typed under ${owner.configPath}, the project that owns their files` +
            (demoted > 0 ? `; ${demoted} demoted because the surface is emitted under ${emit.configPath}` : '')
        );
      }
    }
    return { resolved, errors };
  };
  return {
    ...underEmit((_owner, analysis) => analysis.resolved),
    resolveAgain: (anchors, backfilled) =>
      underEmit((_owner, analysis, indexes) =>
        analysis.resolveAgain(indexes.map((index) => anchors[index]), backfilled).resolved
      ),
  };
}

/**
 * The compiler host a capture program is built on: the Deno graph's, else the
 * default one (`undefined`), each held to the read bound when there is one.
 */
function programHost(
  options: ts.CompilerOptions,
  deno: DenoProject | undefined,
  bound: ReadBound | undefined
): ts.CompilerHost | undefined {
  if (!bound) return deno?.host(options);
  return boundedCompilerHost(deno ? deno.host(options) : ts.createCompilerHost(options), bound);
}

/**
 * Phase A's answer: every anchor resolved, and the program(s) it was read in,
 * held for a literal backfill (carrick#2041).
 */
interface Analysis {
  resolved: ResolvedAnchor[];
  /** What the surface records of how the anchors' projects were read. */
  errors: string[];
  /**
   * The anchors of a backfilled surface (`anchors`, one per anchor, in the
   * same order), read in the same program(s) as a capture of them would read
   * them: the `backfilled` aliases and any literal whose bare name now
   * resolves through another sibling anchor are read again, the rest are
   * answered as before.
   */
  resolveAgain(
    anchors: readonly CaptureAnchorRequest[],
    backfilled: ReadonlySet<string>
  ): { resolved: ResolvedAnchor[]; errors: string[] };
}

/** What Phase A reads beside the anchors and the options they are typed under. */
interface AnalysisContext {
  repoRoot: string;
  entryDir: string;
  entryPath: string;
  guard: WriteGuard;
  progress: (phase: CapturePhase, message: string) => void;
  /** The untrusted-tree read bound, or undefined. */
  bound: ReadBound | undefined;
  /** The augmentation files of a project, see `augmentationReader`. */
  augmentations: (parsed: ts.ParsedCommandLine) => readonly string[];
  /** How an owner's program reads the projects it references, if it references any. */
  redirectFor: (owner: ServiceProject) => ReferenceRedirect | undefined;
  /** Whether the named config references other projects, so owners are read by references. */
  readsReferences: boolean;
}

/**
 * The files of an anchor program the surface emit cannot carry (carrick#2418):
 * the sources of the projects its owner references, and a workspace package's
 * source reached through a link (an external-library file inside the checkout
 * under no `node_modules`). The anchor program types them; the emit, under
 * the compiler's own resolution, reaches the first as unbuilt output and does
 * not emit the second.
 */
function uncarriedFiles(
  program: ts.Program,
  redirect: ReferenceRedirect | undefined,
  repoRoot: string
): (fileName: string) => boolean {
  return (fileName) => {
    if (redirect?.referenceFor(fileName)) return true;
    const sourceFile = program.getSourceFile(fileName);
    const slashed = fileName.replace(/\\/g, '/');
    return (
      sourceFile !== undefined &&
      program.isSourceFileFromExternalLibrary(sourceFile) &&
      isInsideCheckout(slashed, repoRoot) &&
      !slashed.includes('/node_modules/')
    );
  };
}

/**
 * The files a project's config lists that declare a module or global
 * augmentation, for one capture: each project's list is read from disk once,
 * however many programs are built over it.
 *
 * A project's own compile holds these files whether or not anything imports
 * them, so a program that stands in for that compile must hold them too. A
 * file under `node_modules` is the package's business, and a file outside the
 * read bound is not opened, not even to look for the declaration.
 */
function augmentationReader(
  bound: ReadBound | undefined
): (parsed: ts.ParsedCommandLine) => readonly string[] {
  const byProject = new Map<ts.ParsedCommandLine, readonly string[]>();
  return (parsed) => {
    let files = byProject.get(parsed);
    if (!files) {
      files = findAugmentationFiles(
        parsed.fileNames.filter(
          (f) => !f.includes(`${path.sep}node_modules${path.sep}`) && (!bound || bound.allows(f))
        )
      );
      byProject.set(parsed, files);
    }
    return files;
  };
}

/**
 * Phase A: build the placeholder entry, then resolve every anchor. `owner` is
 * the tsconfig project `parsed` came from, when there is one.
 */
function resolveAnchors(
  opts: CaptureStubOptions,
  parsed: ts.ParsedCommandLine,
  ctx: AnalysisContext,
  deno?: DenoProject,
  owner?: ServiceProject,
): Analysis {
  const placeholderLines = ['// Carrick capture v2 analysis placeholder.'];
  for (const anchor of opts.anchors) {
    placeholderLines.push(`export type ${anchor.alias} = unknown;`);
  }

  ctx.guard.writeFile(ctx.entryPath, placeholderLines.join('\n') + '\n');
  try {
    ctx.progress('program', 'building the program the anchors are read in');
    const anchorSources = [
      ...new Set(
        opts.anchors
          .flatMap((a) => (a.source_file ? [path.join(ctx.repoRoot, a.source_file)] : []))
      ),
    ].filter((f) => fs.existsSync(f));
    const options = sidecarCompilerOptions({
      ...parsed.options,
      noEmit: true,
    });
    // The project's augmentation files are roots: its own compile reads a
    // member one of them declares wherever the member is read, imported or not.
    const rootNames = [
      ...new Set([ctx.entryPath, ...anchorSources, ...ctx.augmentations(parsed), ...(deno?.globals ?? [])]),
    ];
    const redirect = owner && !deno ? ctx.redirectFor(owner) : undefined;
    const host = redirect
      ? redirectingCompilerHost(programHost(options, deno, ctx.bound) ?? ts.createCompilerHost(options), options, redirect)
      : programHost(options, deno, ctx.bound);
    const program = ts.createProgram(rootNames, options, host);
    if (owner && !deno && ctx.readsReferences) markUncarried(program, uncarriedFiles(program, redirect, ctx.repoRoot));
    // The checker binds every file of the program when it is first asked for,
    // which the first anchor would otherwise do: asked for here, so that the
    // report below says the program is whole before any anchor is read.
    program.getTypeChecker();
    ctx.progress('anchors', `0 of ${opts.anchors.length}`);
    const entrySource = program.getSourceFile(ctx.entryPath);
    const placeholders = new Map<string, ts.TypeAliasDeclaration>();
    if (entrySource) {
      for (const stmt of entrySource.statements) {
        if (ts.isTypeAliasDeclaration(stmt)) placeholders.set(stmt.name.text, stmt);
      }
    }
    // A module specifier as the entry resolves it, under the program's own
    // resolution (the Deno graph's, for a Deno service). Asked once per
    // specifier: every anchor of a module asks for the same one.
    const entryMode = entrySource?.impliedNodeFormat;
    const fromEntry = new Map<string, string | undefined>();
    const resolveFromEntry = (specifier: string): string | undefined => {
      if (!fromEntry.has(specifier)) {
        fromEntry.set(
          specifier,
          (deno
            ? deno.resolve(specifier, ctx.entryPath, options)
            : ts.resolveModuleName(specifier, ctx.entryPath, options, boundedSys(ctx.bound), undefined, undefined, entryMode)
                .resolvedModule
          )?.resolvedFileName
        );
      }
      return fromEntry.get(specifier);
    };
    // A literal anchor whose text is a bare identifier resolves through a
    // sibling symbol anchor's module when one names the same symbol.
    const siblingSymbolSpecsOf = (anchors: readonly CaptureAnchorRequest[]): Map<string, string> => {
      const specs = new Map<string, string>();
      for (const anchor of anchors) {
        if (anchor.kind !== 'symbol') continue;
        if (!specs.has(anchor.symbol_name)) {
          specs.set(
            anchor.symbol_name,
            entryRelativeSpecifier(ctx.entryDir, ctx.repoRoot, anchor.source_file, resolveFromEntry)
          );
        }
      }
      return specs;
    };
    const siblingSymbolSpecs = siblingSymbolSpecsOf(opts.anchors);
    const resolveIn = (request: CaptureAnchorRequest, specs: Map<string, string>) =>
      resolveAnchor(program, request, {
        repoRoot: ctx.repoRoot,
        entryDir: ctx.entryDir,
        placeholder: placeholders.get(request.alias),
        siblingSymbolSpecs: specs,
        resolveFromEntry,
      });
    const resolved = opts.anchors.map((request, index) => {
      const anchor = resolveIn(request, siblingSymbolSpecs);
      ctx.progress('anchors', `${index + 1} of ${opts.anchors.length}`);
      return anchor;
    });
    return {
      resolved,
      errors: [],
      // The program is held by this closure until the capture drops it.
      resolveAgain: (anchors, backfilled) => {
        const specs = siblingSymbolSpecsOf(anchors);
        return {
          resolved: anchors.map((request, index) => {
            // A backfilled alias is a literal now, and a literal whose bare
            // name a backfilled symbol anchor resolved reads through another
            // sibling, or none. Every other anchor reads as it did.
            const text = request.kind === 'literal' ? request.type_text.trim() : undefined;
            const reread =
              backfilled.has(request.alias) ||
              (text !== undefined && siblingSymbolSpecs.get(text) !== specs.get(text));
            return reread ? resolveIn(request, specs) : resolved[index];
          }),
          errors: [],
        };
      },
    };
  } finally {
    if (fs.existsSync(ctx.entryPath)) ctx.guard.unlink(ctx.entryPath);
  }
}

// carrick#1841: the response-table reading the inferrer and the v1 bundle
// share with the capture's symbol anchor.
export {
  classifyStatusCodes,
  constituents,
  httpStatus,
  statesNoBody,
  statesNoShape,
  statusKeyCode,
  statusTableBody,
  statusTableOf,
  successEntries,
} from './status-table.js';
