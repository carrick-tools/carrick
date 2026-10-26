/**
 * Main entry point for the type-sidecar
 *
 * This module implements a message loop that:
 * 1. Listens on stdin for JSON requests
 * 2. Processes each request (see handleRequest for the actions)
 * 3. Writes JSON responses to stdout
 *
 * IMPORTANT:
 * - stdout is ONLY for JSON responses
 * - stderr is for logging
 * - Process stays alive between requests (warm standby)
 * - One request at a time. A handler that runs the compiler blocks the event
 *   loop until it returns, so a request sent meanwhile waits in the pipe, and
 *   nothing can cancel the one that is running. The scanner kills a process
 *   that goes silent past its deadline; a long handler stays alive by writing
 *   `progress` frames as it finishes units of work (writeProgress).
 */

import * as fs from 'node:fs';
import * as path from 'node:path';
import * as readline from 'node:readline';
import { fileURLToPath } from 'node:url';
import type { Project } from 'ts-morph';
import { parseRequest } from './validators.js';
import { ProjectLoader } from './project-loader.js';
import { TypeBundler } from './bundler.js';
import { TypeInferrer } from './type-inferrer.js';
import { DefinitionResolver } from './definition-resolver.js';
import {
  CaptureHold,
  captureStub,
  findDisqualifyingTopTypes,
  jsonWireDeclarations,
  KeptJudge,
  runCheck,
  type EmitReach,
} from './capture/index.js';
import { Retyper, type TopTypeWalk } from './retype.js';
import { LibraryClaimsVerifier, httpCheck, type LabelledMembers } from './library-claims.js';
import { readServerLabels } from './server-labels.js';
import { PROGRESS_INTERVAL_MS, atMostEvery } from './progress.js';
import { mergeInferTimings } from './infer-timing.js';
import { addProgramFiles, listProgramFiles } from './program-files.js';
import {
  type AnswerKey,
  type KeptAnswer,
  changedSource,
  inferCacheDir,
  keepable,
  programDifference,
  programRecord,
  rebuildProgram,
  readEntry,
  requestDigest,
  writeEntry,
} from './infer-cache.js';
import { writePositionRows } from './position-rows.js';
import { findImplementations } from './implementations.js';
import { HeldFiles, answerWhileStable } from './held-freshness.js';
import {
  affectedFiles,
  importGraphOf,
  readAgain,
  readsOf,
  type FileChange,
  type ImportGraph,
  type Reached,
} from './affected-set.js';
import { couplingOf, dropReached, itemKeysOf, sameCoupling, type HeldItem } from './held-items.js';
import {
  Connection,
  DEFAULT_IDLE_MS,
  EXIT_ALREADY_RUNNING,
  TypedServer,
  buildIdOf,
} from './typed-server.js';
import { newVerifyCounts, spawnColdRunner, verifyAnswer, verifyEnabled, type ColdRunner } from './typed-verify.js';
import type {
  BundleResult,
  RetypeOutcome,
  SidecarRequest,
  SidecarResponse,
  InitResponse,
  BundleResponse,
  CaptureV2Response,
  CheckV2Response,
  InferResponse,
  ResolveDefinitionsResponse,
  RetypeCheckResponse,
  VerifyClientSemanticsResponse,
  VerifyLibraryClaimsResponse,
  ListLibrarySurfaceResponse,
  HealthResponse,
  ShutdownResponse,
  ListProgramFilesResponse,
  AddProgramFilesResponse,
  PositionRowsResponse,
  ImplementationsResponse,
  StatusResponse,
  ErrorResponse,
  ServerClaims,
  InferRequestItem,
  InferItemOutcome,
  InferTiming,
} from './types.js';

// ===========================================================================
// Module-level state
// ===========================================================================

/**
 * The components that read the init'd ts-morph project. Built together with
 * the project on first use and dropped on re-init, so re-scoping the sidecar
 * to another service can never serve the previous service's program.
 */
interface ProjectComponents {
  typeBundler: TypeBundler;
  typeInferrer: TypeInferrer;
  retyper: Retyper;
  claimsVerifier: LibraryClaimsVerifier;
  /**
   * carrick#2674: the members a set of server claims names for this program,
   * by the claims' text. Read once: each reading builds a program of its own.
   */
  labelledMembers: Map<string, LabelledMembers>;
}

/** One init'd service: its project loader and everything built over it. */
interface Session {
  projectLoader: ProjectLoader | null;
  initTimeMs: number | null;
  /** `requestDigest` of the `init` request it was scoped with (carrick#2843). */
  initDigest: string;
  /**
   * Components by project key (`ProjectLoader.projectKeyFor`): `''` is the
   * default project, any other key the program of a project that owns some of
   * the service's files (carrick#1604).
   */
  components: Map<string, ProjectComponents>;
  /** The roots its `init` named, as `status` reports them. */
  scope: { repo_root: string; scan_root: string | null } | null;
  /** Server mode: the `init` that built it, to build it again from (carrick#2880). */
  init: (SidecarRequest & { action: 'init' }) | null;
  /** Server mode: the files its programs were built from, as last seen. */
  held: HeldFiles | null;
  /** Server mode: how often a sweep refreshed it or dropped it. */
  freshness: { refreshes: number; drops: number };
  /**
   * Server mode: each `infer` item's answer, by item key, kept to answer it
   * again while no edit reaches it (carrick#2881). Null over stdio.
   */
  items: Map<string, HeldItem> | null;
  /** Server mode: `infer` items answered from `items`, and read again. */
  answers: { replayed: number; recomputed: number };
  /** Server mode: how often it was evicted to keep the heap under its cap. */
  evictions: number;
  /** Server mode: when it last answered a request (`performance.now()`). */
  lastUsed: number;
  /**
   * Server mode: each held project's import graph, carried across edits with
   * the files an edit changed read again on first use (carrick#2881).
   */
  graphs: Map<Project, { graph: ImportGraph; stale: Set<string> }>;
}

function newSession(): Session {
  return {
    projectLoader: null,
    initTimeMs: null,
    initDigest: '',
    components: new Map(),
    scope: null,
    init: null,
    held: null,
    freshness: { refreshes: 0, drops: 0 },
    items: null,
    answers: { replayed: 0, recomputed: 0 },
    evictions: 0,
    lastUsed: 0,
    graphs: new Map(),
  };
}

/**
 * The service the request being handled reads. Over stdio there is one, and
 * re-init replaces what it holds. In server mode each connection selects the
 * held session its `init` names before each of its requests (carrick#2879).
 */
let session: Session = newSession();

/**
 * Reads a capture stub's own declaration tree and nothing of the init'd
 * project, so it is not one of the project's components: taking it from them
 * would build the service's whole program to answer about the stub
 * (carrick#1927).
 */
const definitionResolver = new DefinitionResolver();

/** The init'd project loader, or the error every project-backed action gives without one. */
function initializedLoader(): ProjectLoader {
  if (!session.projectLoader?.isInitialized()) {
    throw new Error('Sidecar not initialized. Call init first.');
  }
  return session.projectLoader;
}

/**
 * carrick#2100: the unwidened reading's budget in milliseconds, when
 * `CARRICK_UNWIDENED_BUDGET_MS` sets one. A switch for tests that drive the
 * scanner's binary: it forces the budget to run out on any machine. Unset or
 * not a whole number of milliseconds, the inferrer's default stands.
 */
function unwidenedBudgetFromEnv(): number | undefined {
  const value = process.env.CARRICK_UNWIDENED_BUDGET_MS;
  if (value === undefined || !/^\d+$/.test(value.trim())) return undefined;
  return Number(value.trim());
}

/**
 * Get the project-backed components, building the project if this is the
 * first request that needs it.
 *
 * @throws if init has not run, or if the project cannot be built
 */
function projectComponents(key = ''): ProjectComponents {
  // Bound here rather than read from the module slot inside the components:
  // a re-init drops `components` and points the slot at another service, and
  // nothing built over this project may follow it there.
  const loader = initializedLoader();
  let built = session.components.get(key);
  if (!built) {
    const project = loader.getProjectFor(key);
    const repoRoot = loader.getRepoRoot();
    // The module graph, where the project resolved through one, is the only
    // thing that can name the package a file belongs to: a Deno service
    // resolves nothing under `node_modules` (carrick#1260).
    const typeInferrer = new TypeInferrer({
      project,
      repoRoot,
      packageOf: (filePath) => loader.packageOf(filePath),
      unwidenedBudgetMs: unwidenedBudgetFromEnv(),
    });
    built = {
      typeBundler: new TypeBundler({ project, repoRoot }),
      typeInferrer,
      // Locates calls exactly as `infer` does, so it rewrites the node the
      // consumer's published type came from (carrick#1491).
      // The walk is typed against the capture bundle's compiler copy and
      // handed this project's: it reads only `TypeFlags` and `ObjectFlags`,
      // which the two copies share (pinned in retype-top-types.test.ts). The
      // pin runs against this package's lockfile; the published package
      // (`npm/carrick/package.json`) resolves `typescript ^5.8.0` unpinned,
      // so an install can pair ts-morph's copy with a newer 5.x.
      retyper: new Retyper(
        project,
        typeInferrer,
        jsonWireDeclarations,
        findDisqualifyingTopTypes as unknown as TopTypeWalk
      ),
      claimsVerifier: new LibraryClaimsVerifier(project, {
        packageDirectoryOf: (file) => loader.packageDirectoryOf(file),
        resolveModule: (name, from, options) => loader.resolveThroughGraph(name, from, options),
      }),
      labelledMembers: new Map(),
    };
    session.components.set(key, built);
  }
  return built;
}

/** One bundle answer from the answers of several programs. */
function mergeBundles(results: BundleResult[]): BundleResult {
  const symbol_failures = results.flatMap((r) => r.symbol_failures ?? []);
  const answered = results.filter((r) => r.success);
  if (answered.length === 0) {
    return {
      success: false,
      symbol_failures,
      errors: [...new Set(results.flatMap((r) => r.errors ?? []))],
    };
  }
  return {
    success: true,
    dts_content: answered.map((r) => r.dts_content ?? '').join('\n'),
    manifest: answered.flatMap((r) => r.manifest ?? []),
    symbol_failures: symbol_failures.length > 0 ? symbol_failures : undefined,
  };
}

/**
 * Split a request's items by the project that types their file, keeping each
 * group in request order. One group, the default project's, for a service
 * whose tsconfig references nothing.
 */
function byProject<T>(items: T[], fileOf: (item: T) => string | undefined): Map<string, T[]> {
  // An empty request is still answered by the default project, as before.
  const groups = new Map<string, T[]>(items.length === 0 ? [['', []]] : []);
  for (const item of items) {
    const key = session.projectLoader?.projectKeyFor(fileOf(item)) ?? '';
    groups.set(key, [...(groups.get(key) ?? []), item]);
  }
  return groups;
}

// ===========================================================================
// Request Handlers
// ===========================================================================

/**
 * Handle the 'init' action - initialize the TypeScript project
 */
function handleInit(request: SidecarRequest & { action: 'init' }): InitResponse {
  const startTime = performance.now();

  try {
    log(`Initializing with repo_root: ${request.repo_root}`);

    // Re-init re-scopes the sidecar to another root: drop everything built
    // over the previous project before resolving the new one.
    session.components = new Map();
    session.initDigest = requestDigest(request);
    session.projectLoader = new ProjectLoader({
      repoRoot: request.repo_root,
      tsconfigPath: request.tsconfig_path,
      scanRoot: request.scan_root,
      tsconfigSnapshot: request.tsconfig_snapshot,
      pinnedDependencies: request.pinned_dependencies,
    });

    const result = session.projectLoader.load();

    if (!result.success) {
      return {
        request_id: request.request_id,
        status: 'error',
        errors: [result.error || 'Unknown initialization error'],
        init_time_ms: result.initTimeMs,
      };
    }

    // The project and everything that reads it are built by the first request
    // that needs them, so readiness costs the same on a bare checkout as on
    // one with its dependencies installed (carrick#749).

    session.initTimeMs = result.initTimeMs || Math.round(performance.now() - startTime);

    log(`Initialization complete in ${session.initTimeMs}ms`);

    return {
      request_id: request.request_id,
      status: 'ready',
      init_time_ms: session.initTimeMs,
    };
  } catch (err) {
    const error = err instanceof Error ? err.message : String(err);
    logError(`Initialization failed: ${error}`);

    return {
      request_id: request.request_id,
      status: 'error',
      errors: [error],
      init_time_ms: Math.round(performance.now() - startTime),
    };
  }
}

/**
 * Handle the 'bundle' action - bundle explicit types (legacy)
 */
function handleBundle(request: SidecarRequest & { action: 'bundle' }): BundleResponse {
  try {
    log(`Bundling ${request.symbols.length} symbol(s)`);

    // A symbol named by a path is bundled from the program of the project
    // that owns that file (carrick#1604).
    const results = [...byProject(request.symbols, (symbol) => symbol.source_file)].map(
      ([key, symbols]) => {
        const { typeBundler } = projectComponents(key);
        writeProgress(request.request_id, 'bundle', 'program ready');
        return typeBundler.bundle(symbols);
      }
    );
    const result = results.length === 1 ? results[0] : mergeBundles(results);

    if (!result.success) {
      return {
        request_id: request.request_id,
        status: 'error',
        dts_content: result.dts_content,
        manifest: result.manifest,
        symbol_failures: result.symbol_failures,
        errors: result.errors,
      };
    }

    return {
      request_id: request.request_id,
      status: 'success',
      dts_content: result.dts_content,
      manifest: result.manifest,
      symbol_failures: result.symbol_failures,
    };
  } catch (err) {
    const error = err instanceof Error ? err.message : String(err);
    logError(`Bundle failed: ${error}`);

    return {
      request_id: request.request_id,
      status: 'error',
      errors: [error],
    };
  }
}

/**
 * Handle the 'capture_v2' action - v2 "tsc as serializer" capture.
 * Stateless by design: unlike bundle/infer it needs no init'd ts-morph
 * project, only the repo's own tsconfig — this is the point of v2. The
 * capture bundle behind this action is the seam ("seam, not split"): this
 * dispatcher call is the only non-type reach-in.
 */
function handleCaptureV2(request: SidecarRequest & { action: 'capture_v2' }): CaptureV2Response {
  try {
    log(`capture_v2 for service '${request.service_name}' (${request.anchors.length} anchor(s))`);
    // A frame for each stage as the capture reaches it, and within a stage
    // (one report per anchor) at most one per interval.
    let stage: string | undefined;
    const withinStage = atMostEvery(PROGRESS_INTERVAL_MS, (phase: string, message: string) =>
      writeProgress(request.request_id, phase, message)
    );
    const result = captureStub(
      {
      repoRoot: request.repo_root,
      serviceName: request.service_name,
      anchors: request.anchors,
      outDir: request.out_dir,
      tsconfigPath: request.tsconfig_path,
      scanRoot: request.scan_root,
      onProgress: (phase, message) => {
        if (phase === stage) return withinStage(phase, message);
        stage = phase;
        writeProgress(request.request_id, phase, message);
      },
      },
      captureHoldFor(request)
    );
    return {
      request_id: request.request_id,
      status: result.success ? 'success' : 'error',
      result,
      errors: result.errors.length > 0 ? result.errors : undefined,
    };
  } catch (err) {
    const error = err instanceof Error ? err.message : String(err);
    logError(`capture_v2 failed: ${error}`);
    return {
      request_id: request.request_id,
      status: 'error',
      errors: [error],
    };
  }
}

/**
 * Handle the 'check_v2' action - v2 "tsc as judge" compatibility check.
 *
 * Async by necessity: the vendored pnpm install can exceed the Rust client's
 * read deadline, and running it off the event loop (spawn, not execSync) keeps
 * the sidecar responsive. Emits `status: 'progress'` keepalive frames during
 * install/check and one terminal `success`/`error` frame. Writes its own
 * frames, so processLine hands off and does not write a response for it.
 */
/**
 * A daemon's judge workspace, kept between checks (carrick#2883). Over stdio
 * every check builds its own, as before.
 */
let keptJudge: KeptJudge | null = null;

async function handleCheckV2Async(
  request: SidecarRequest & { action: 'check_v2' }
): Promise<void> {
  const out = sink;
  let phase = 'assembling';
  const keepalive = setInterval(() => {
    writeProgress(request.request_id, phase, `check ${phase}`, out);
  }, 1500);
  keepalive.unref();
  try {
    log(`check_v2: ${request.stubs.length} stub(s), ${request.pairs.length} pair(s)`);
    const result = await runCheck(
      {
        stubs: request.stubs,
        pairs: request.pairs,
        workspaceRoot: request.workspace_root,
        cleanup: request.keep_workspace !== true,
        // A caller that asks for the workspace gets a fresh one of its own.
        kept: request.keep_workspace === true ? undefined : keptJudge ?? undefined,
      },
      (p) => {
        phase = p;
      }
    );
    clearInterval(keepalive);
    const response: CheckV2Response = {
      request_id: request.request_id,
      status: result.success ? 'success' : 'error',
      result,
      errors: result.errors.length > 0 ? result.errors : undefined,
    };
    writeResponse(response, out);
  } catch (err) {
    clearInterval(keepalive);
    const error = err instanceof Error ? err.message : String(err);
    logError(`check_v2 failed: ${error}`);
    writeResponse(
      {
        request_id: request.request_id,
        status: 'error',
        errors: [error],
      },
      out
    );
  }
}

/**
 * Handle the 'infer' action - infer implicit types
 */
function handleInfer(request: SidecarRequest & { action: 'infer' }): InferResponse {
  try {
    log(`Inferring ${request.requests.length} type(s)`);

    const groups = byProject(request.requests, (item) => item.file_path);
    // A daemon answers from the items it keeps, never from the kept file.
    if (session.items) return inferHeld(request, groups, session.items);
    const cacheKey = inferAnswerKey(request, groups);
    if (cacheKey) {
      const served = servedAnswer(cacheKey);
      if (served) {
        log(`Inferred ${request.requests.length} type(s) from the kept answer (carrick#2843)`);
        return { request_id: request.request_id, ...served };
      }
    }

    // One count for the request, whichever programs its items span.
    let done = 0;
    const report = atMostEvery(PROGRESS_INTERVAL_MS, () =>
      writeProgress(request.request_id, 'infer', `${done} of ${request.requests.length}`)
    );
    const results = [...groups].map(
      ([key, items]) => {
        const built = projectComponents(key);
        return built.typeInferrer.infer(
          items,
          request.extraction_config,
          () => {
            done += 1;
            report();
          },
          request.model_symbols,
          labelledMembersFor(built, key, items, request.server_claims)
        );
      }
    );
    const result =
      results.length === 1
        ? results[0]
        : (() => {
            const inferred_types = results.flatMap((r) => r.inferred_types ?? []);
            const errors = results.flatMap((r) => r.errors ?? []);
            return {
              success: errors.length === 0 || inferred_types.length > 0,
              inferred_types,
              errors: errors.length > 0 ? errors : undefined,
            };
          })();

    const answer: KeptAnswer = {
      status: result.success ? 'success' : 'error',
      inferred_types: result.inferred_types,
      errors: result.errors,
    };
    if (cacheKey) keepAnswer(cacheKey, [...groups.keys()], answer);
    return {
      request_id: request.request_id,
      ...answer,
      // Beside the answers and never in them: what the requests cost, for
      // the caller's log (carrick#1985).
      infer_timing: mergeInferTimings(results.map((r) => r.timing)),
    };
  } catch (err) {
    const error = err instanceof Error ? err.message : String(err);
    logError(`Inference failed: ${error}`);

    return {
      request_id: request.request_id,
      status: 'error',
      errors: [error],
    };
  }
}

/**
 * carrick#2881: answer `infer` in a daemon, reading again only the items it
 * keeps no answer for (`held-items.ts`) and taking the rest from `items`.
 * Each program's items are read in request order, in one batch, as a cold
 * process reads them; an item coupled to one read again is read again with it.
 */
function inferHeld(
  request: SidecarRequest & { action: 'infer' },
  groups: Map<string, InferRequestItem[]>,
  items: Map<string, HeldItem>
): InferResponse {
  const keys = itemKeysOf(request as unknown as { requests: InferRequestItem[] } & Record<string, unknown>);
  const indexOf = new Map(request.requests.map((item, index) => [item, index]));
  const outcomes = new Array<InferItemOutcome>(request.requests.length);
  const timings: InferTiming[] = [];
  let readAgainCount = 0;
  let done = 0;
  const report = atMostEvery(PROGRESS_INTERVAL_MS, () =>
    writeProgress(request.request_id, 'infer', `${done} of ${request.requests.length}`)
  );
  for (const [key, group] of groups) {
    const indices = group.map((item) => indexOf.get(item)!);
    const readAgain = new Set(indices.filter((index) => !items.has(keys[index]!)));
    const fresh = new Map<number, Set<string>>();
    for (;;) {
      if (readAgain.size > 0) {
        const chosen = indices.filter((index) => readAgain.has(index));
        const asked = chosen.map((index) => request.requests[index]!);
        const built = projectComponents(key);
        const result = built.typeInferrer.infer(
          asked,
          request.extraction_config,
          () => {
            done += 1;
            report();
          },
          request.model_symbols,
          labelledMembersFor(built, key, asked, request.server_claims)
        );
        timings.push(result.timing);
        chosen.forEach((index, at) => {
          outcomes[index] = result.items[at]!;
        });
        fresh.clear();
        const reading = chosen.filter((index) => (outcomes[index]!.pathFiles?.length ?? 0) > 0);
        if (reading.length > 0) {
          const project = initializedLoader().getProjectFor(key);
          const program = project.getProgram().compilerObject;
          const graph = graphFor(project);
          for (const index of reading) {
            const asked = path.resolve(initializedLoader().getRepoRoot(), request.requests[index]!.file_path);
            fresh.set(index, readsOf(graph, program.getSourceFile(asked)?.fileName ?? asked));
          }
        }
      }
      const coupled = couplingOf(
        indices.map((index) =>
          readAgain.has(index)
            ? { key: keys[index]!, pathFiles: outcomes[index]!.pathFiles, reads: fresh.get(index) }
            : { key: keys[index]!, pathFiles: items.get(keys[index]!)!.outcome.pathFiles, reads: items.get(keys[index]!)!.reads }
        )
      );
      const readAgainKeys = new Set([...readAgain].map((index) => keys[index]!));
      const moved = indices.filter(
        (index) =>
          !readAgain.has(index) &&
          !sameCoupling(items.get(keys[index]!)!.coupled, coupled.get(keys[index]!) ?? [], readAgainKeys)
      );
      if (moved.length === 0) {
        for (const index of readAgain) {
          items.set(keys[index]!, {
            file: heldFileOf(request.requests[index]!),
            outcome: outcomes[index]!,
            coupled: coupled.get(keys[index]!) ?? [],
            reads: fresh.get(index),
          });
        }
        for (const index of indices) {
          if (!readAgain.has(index)) outcomes[index] = items.get(keys[index]!)!.outcome;
        }
        session.answers.replayed += indices.length - readAgain.size;
        session.answers.recomputed += readAgain.size;
        readAgainCount += readAgain.size;
        break;
      }
      for (const index of moved) readAgain.add(index);
    }
  }
  // Assembled as `handleInfer` assembles a cold answer: by program, in request order within each.
  const ordered = [...groups.values()].flatMap((group) => group.map((item) => outcomes[indexOf.get(item)!]!));
  const inferred_types = ordered.flatMap((outcome) => (outcome.inferred ? [outcome.inferred] : []));
  const errors = ordered.flatMap((outcome) => outcome.errors);
  const success = errors.length === 0 || inferred_types.length > 0;
  log(`Inferred ${request.requests.length} type(s): ${readAgainCount} read again, the rest kept (carrick#2881)`);
  return {
    request_id: request.request_id,
    status: success ? 'success' : 'error',
    inferred_types: inferred_types.length > 0 ? inferred_types : undefined,
    errors: errors.length > 0 ? errors : undefined,
    infer_timing: mergeInferTimings(timings),
  };
}

/**
 * An item's file by its real path: a program names a root file as its config
 * lists it and a file reached through a link by its real path, so kept items
 * and the files an edit reaches meet on the real path.
 */
function heldFileOf(item: InferRequestItem): string {
  return realFileOf(path.resolve(initializedLoader().getRepoRoot(), item.file_path));
}

function realFileOf(file: string): string {
  try {
    return fs.realpathSync.native(file);
  } catch {
    return file;
  }
}

/**
 * carrick#2843: what a kept answer to `request` is read against, or
 * `undefined` when no answer to it is kept: the cache is off, the request
 * reads server-framework claims (in a program the record does not cover),
 * the project resolves through a module graph read outside the compiler, or
 * a request before this one built a program it is answered in (its roots
 * then depend on that request).
 */
function inferAnswerKey(
  request: SidecarRequest & { action: 'infer' },
  groups: Map<string, InferRequestItem[]>
): AnswerKey | undefined {
  const dir = inferCacheDir();
  if (!dir) return undefined;
  const loader = initializedLoader();
  if (loader.resolvesThroughModuleGraph()) return undefined;
  for (const [key, items] of groups) {
    if (loader.isBuilt(key) || session.components.has(key)) return undefined;
    if (
      request.server_claims &&
      request.server_claims.checks.length > 0 &&
      items.some((item) => ROUTE_ROW_KINDS.has(item.infer_kind))
    ) {
      return undefined;
    }
  }
  return { dir, repoRoot: loader.getRepoRoot(), init: session.initDigest, request: requestDigest(request) };
}

/**
 * carrick#2843: the kept answer for `key` when every program it was read in
 * is, built again now, the same program. Each is built from its tsconfig and
 * given the roots the kept one had, in order, and rebuilt once more when the
 * kept one had been, so a match leaves the program the inference would have
 * left. A program that does not match is dropped
 * again, so the inference that follows builds it as it would have with no
 * cache.
 */
function servedAnswer(key: AnswerKey): KeptAnswer | undefined {
  const entry = readEntry(key);
  if (!entry) return undefined;
  // A file that changed on disk is found without building a program.
  for (const kept of entry.programs) {
    const changedFile = changedSource(kept);
    if (changedFile) {
      log(`The kept infer answer was read in a program that has changed (${changedFile}): inferring again (carrick#2843)`);
      return undefined;
    }
  }
  const loader = initializedLoader();
  const touched: string[] = [];
  let same = true;
  let changed = 'a root file it could not read';
  for (const kept of entry.programs) {
    touched.push(kept.key);
    const project = loader.getProjectFor(kept.key);
    addProgramFiles(project, kept.roots, () => {
      same = false;
    });
    if (!same) break;
    let now = programRecord(kept.key, project);
    if (now.roots.length < kept.roots.length) {
      // The inference's own rewrites rebuilt the program after its first
      // build, and a rebuild names every file the first one reached as a
      // root. Rebuilt once more here, it names the same.
      rebuildProgram(project);
      now = programRecord(kept.key, project);
    }
    const difference = programDifference(kept, now);
    if (difference) {
      same = false;
      changed = difference;
      break;
    }
  }
  if (same) return entry.answer;
  for (const programKey of touched) loader.releaseProjectFor(programKey);
  log(`The kept infer answer was read in a program that has changed (${changed}): inferring again (carrick#2843)`);
  return undefined;
}

/** carrick#2843: keep `answer` for `key`, with the programs it was read in as they stand. */
function keepAnswer(key: AnswerKey, programKeys: string[], answer: KeptAnswer): void {
  if (!keepable(answer)) return;
  try {
    const loader = initializedLoader();
    writeEntry(
      key,
      programKeys.map((programKey) => programRecord(programKey, loader.getProjectFor(programKey))),
      answer
    );
  } catch (err) {
    // Keeping an answer never costs the answer.
    logError(`The infer answer was not kept: ${err instanceof Error ? err.message : String(err)}`);
  }
}

/**
 * How long reading one program's server-framework labels may take: as
 * long as a verify_library_claims request, since the reading builds a
 * program of its own. Labels it does not reach are not read.
 */
const SERVER_CLAIMS_BUDGET_MS = 600_000;

/** The infer kinds a route row asks: only these read a handler's parameters (carrick#2674). */
const ROUTE_ROW_KINDS: ReadonlySet<string> = new Set(['response_body', 'request_body']);

/**
 * carrick#2674: the members an `infer` request's server-framework claims
 * name, verified again in a program of their own beside the inferrer's for
 * `key` (`readServerLabels`). Undefined when the request carries none, or
 * the group asks no route row, so the inferrer reads nothing new. Read once
 * per program and claims.
 */
function labelledMembersFor(
  built: ProjectComponents,
  key: string,
  items: readonly InferRequestItem[],
  claims: ServerClaims | undefined
): LabelledMembers | undefined {
  if (!claims || claims.checks.length === 0) return undefined;
  if (!items.some((item) => ROUTE_ROW_KINDS.has(item.infer_kind))) return undefined;
  const cacheKey = JSON.stringify(claims);
  let labels = built.labelledMembers.get(cacheKey);
  if (!labels) {
    const loader = initializedLoader();
    labels = readServerLabels(
      {
        getRepoRoot: () => loader.getRepoRoot(),
        scriptProjectFor: (owner) => loader.scriptProjectFor(owner),
        packageDirectoryOf: (file) => loader.packageDirectoryOf(file),
        resolveModule: (name, from, options) => loader.resolveThroughGraph(name, from, options),
      },
      key,
      claims,
      SERVER_CLAIMS_BUDGET_MS,
      log
    );
    built.labelledMembers.set(cacheKey, labels);
  }
  return labels;
}

/**
 * How long one retype_check request may spend before the items it has not
 * reached abstain: well inside the scanner's 900s read deadline.
 */
const RETYPE_BUDGET_MS = 600_000;

/**
 * Handle the 'retype_check' action - judge untyped consumer calls by retyping
 * them with the producer's response type (carrick#1491)
 */
function handleRetypeCheck(
  request: SidecarRequest & { action: 'retype_check' }
): RetypeCheckResponse {
  try {
    log(`Retyping ${request.items.length} consumer call(s)`);
    const budget = request.budget_ms ?? RETYPE_BUDGET_MS;
    const groups = byProject(
      request.items.map((item, index) => ({ item, index })),
      ({ item }) => item.file_path
    );
    const deadline = performance.now() + budget;
    const outcomes = new Array<RetypeOutcome>(request.items.length);
    // One count for the request, whichever programs its items span, reported
    // as `infer` reports its batch (carrick#1914, carrick#1945).
    let judged = 0;
    const report = atMostEvery(PROGRESS_INTERVAL_MS, () =>
      writeProgress(request.request_id, 'retype', `${judged} of ${request.items.length}`)
    );
    const onJudged = () => {
      judged += 1;
      report();
    };
    for (const [key, entries] of groups) {
      // One budget for the request, whichever programs it spans.
      const remaining = groups.size === 1 ? budget : Math.max(0, deadline - performance.now());
      projectComponents(key)
        .retyper.run(entries.map(({ item }) => item), remaining, onJudged)
        .forEach((outcome, position) => {
          outcomes[entries[position].index] = outcome;
        });
    }
    return { request_id: request.request_id, status: 'success', outcomes };
  } catch (err) {
    const error = err instanceof Error ? err.message : String(err);
    logError(`Retype check failed: ${error}`);
    return { request_id: request.request_id, status: 'error', errors: [error] };
  }
}

/**
 * How long one verify_client_semantics or verify_library_claims request may
 * spend before the checks it has not reached come back `unchecked` with reason
 * `budget`. The first request on a service may pay the program build, which
 * counts against it; the checks themselves are cheap. Well inside the
 * scanner's 900s read deadline.
 */
const SEMANTICS_BUDGET_MS = 600_000;

/**
 * Handle the 'verify_client_semantics' action - check HTTP client-library
 * claims against each package's type declarations (carrick#1564). Each check
 * is converted into the shared claim shape and answered by the same verifier
 * as `verify_library_claims`, with the #1564 checks and reasons.
 */
function handleVerifyClientSemantics(
  request: SidecarRequest & { action: 'verify_client_semantics' }
): VerifyClientSemanticsResponse {
  try {
    const { claimsVerifier } = projectComponents();
    const fromDir = path.resolve(session.projectLoader!.getRepoRoot(), request.from_dir);
    log(`Verifying ${request.checks.length} client-semantics claim(s) from ${fromDir}`);
    const { semantics, modules } = claimsVerifier.run(
      fromDir,
      request.checks.map(httpCheck),
      request.budget_ms ?? SEMANTICS_BUDGET_MS
    );
    return {
      request_id: request.request_id,
      status: 'success',
      semantics,
      semantics_modules: modules,
    };
  } catch (err) {
    const error = err instanceof Error ? err.message : String(err);
    logError(`Client-semantics check failed: ${error}`);
    return { request_id: request.request_id, status: 'error', errors: [error] };
  }
}

/**
 * Handle the 'verify_library_claims' action - check library claims of every
 * role against each package's own declarations (carrick#1616). One verdict
 * per check, in request order.
 */
function handleVerifyLibraryClaims(
  request: SidecarRequest & { action: 'verify_library_claims' }
): VerifyLibraryClaimsResponse {
  const started = performance.now();
  try {
    const { claimsVerifier } = projectComponents();
    // The verifier's budget starts now, and so does the scanner's wait.
    writeProgress(request.request_id, 'verify_library_claims', 'program ready');
    const fromDir = path.resolve(session.projectLoader!.getRepoRoot(), request.from_dir);
    log(`Verifying ${request.checks.length} library claim(s) from ${fromDir}`);
    const { semantics, modules } = claimsVerifier.run(
      fromDir,
      request.checks,
      request.budget_ms ?? SEMANTICS_BUDGET_MS,
      request.scan_root
    );
    return {
      request_id: request.request_id,
      status: 'success',
      verdicts: semantics,
      modules,
      duration_ms: Math.round(performance.now() - started),
    };
  } catch (err) {
    const error = err instanceof Error ? err.message : String(err);
    logError(`Library-claims check failed: ${error}`);
    return { request_id: request.request_id, status: 'error', errors: [error] };
  }
}

/** Entries a surface listing keeps per package when the request names no cap. */
const SURFACE_MAX_ENTRIES = 1000;

/**
 * Handle the 'list_library_surface' action - each specifier's declared
 * surface, read with the verifier's predicates, and the full-surface hash
 * (carrick#1660).
 */
function handleListLibrarySurface(
  request: SidecarRequest & { action: 'list_library_surface' }
): ListLibrarySurfaceResponse {
  try {
    const { claimsVerifier } = projectComponents();
    const fromDir = path.resolve(session.projectLoader!.getRepoRoot(), request.from_dir);
    log(`Listing the declared surface of ${request.packages.length} specifier(s) from ${fromDir}`);
    const { surfaces, surface_sha256 } = claimsVerifier.listSurface(
      fromDir,
      request.packages,
      request.max_entries ?? SURFACE_MAX_ENTRIES,
      request.exports ?? {}
    );
    return { request_id: request.request_id, status: 'success', surfaces, surface_sha256 };
  } catch (err) {
    const error = err instanceof Error ? err.message : String(err);
    logError(`Surface listing failed: ${error}`);
    return { request_id: request.request_id, status: 'error', errors: [error] };
  }
}

/**
 * Handle the 'resolve_definitions' action - resolve surface aliases from a
 * v2 capture stub package's declaration tree. Stateless, as `capture_v2` is:
 * it needs no init and builds no project but the stub's own.
 */
function handleResolveDefinitions(
  request: SidecarRequest & { action: 'resolve_definitions' },
): ResolveDefinitionsResponse {
  try {
    log(`Resolving ${request.aliases.length} type alias(es) from ${request.stub_dir}`);

    const results = definitionResolver.resolveFromStub(request.stub_dir, request.aliases);

    return {
      request_id: request.request_id,
      status: 'success',
      definitions: results.map((r) => ({
        type_alias: r.type_alias,
        definition: r.definition,
        expanded: r.expanded,
      })),
    };
  } catch (err) {
    const error = err instanceof Error ? err.message : String(err);
    logError(`Definition resolution failed: ${error}`);

    return {
      request_id: request.request_id,
      status: 'error',
      errors: [error],
    };
  }
}

/**
 * Handle the 'list_program_files' action - the root files the default
 * project's program was built from, in order (carrick#2027). It is the only
 * program a request grows: a file a referenced project owns is in that
 * project's program from its tsconfig, so a program built for an owning
 * project (carrick#1604) holds the same files in every process. A process
 * that has not built the default project lists nothing, and does not build
 * it to answer.
 */
function handleListProgramFiles(
  request: SidecarRequest & { action: 'list_program_files' }
): ListProgramFilesResponse {
  try {
    const project = initializedLoader().builtProject();
    const files = project ? listProgramFiles(project) : [];
    log(`Listing ${files.length} program file(s)`);
    return { request_id: request.request_id, status: 'success', files };
  } catch (err) {
    const error = err instanceof Error ? err.message : String(err);
    logError(`Listing program files failed: ${error}`);
    return { request_id: request.request_id, status: 'error', errors: [error] };
  }
}

/**
 * Handle the 'add_program_files' action - add the given files to the default
 * project's program in the given order, rebuilding it at most once
 * (carrick#2027). A file a referenced project owns is skipped, as an `infer`
 * on it would never add it to this program: its owner's program already holds
 * it.
 */
function handleAddProgramFiles(
  request: SidecarRequest & { action: 'add_program_files' }
): AddProgramFilesResponse {
  try {
    const loader = initializedLoader();
    const startTime = performance.now();
    const files = request.files
      .map((file) => path.resolve(loader.getRepoRoot(), file))
      .filter((file) => loader.projectKeyFor(file) === '');
    const added =
      files.length === 0
        ? 0
        : addProgramFiles(loader.getProject(), files, (file, reason) =>
            logError(`Not added to the program: ${file}: ${reason}`)
          );
    log(
      `Added ${added} of ${request.files.length} file(s) to the program in ` +
        `${Math.round(performance.now() - startTime)}ms`
    );
    return { request_id: request.request_id, status: 'success', added };
  } catch (err) {
    const error = err instanceof Error ? err.message : String(err);
    logError(`Adding program files failed: ${error}`);
    return { request_id: request.request_id, status: 'error', errors: [error] };
  }
}

/**
 * Handle the 'implementations' action - the implementations of the bodyless
 * declaration each call goes through (carrick#2432). Asked of the analysis
 * process: it reads the programs the service's other actions read, and
 * releases none of them.
 */
function handleImplementations(
  request: SidecarRequest & { action: 'implementations' }
): ImplementationsResponse {
  try {
    const loader = initializedLoader();
    let done = 0;
    const report = atMostEvery(PROGRESS_INTERVAL_MS, () =>
      writeProgress(request.request_id, 'implementations', `${done} of ${request.queries.length}`)
    );
    const answers = findImplementations({
      loader,
      scanRoot: request.scan_root,
      queries: request.queries,
      onAnswer: () => {
        done += 1;
        report();
      },
    });
    return { request_id: request.request_id, status: 'success', result: { answers } };
  } catch (err) {
    const error = err instanceof Error ? err.message : String(err);
    logError(`Implementations failed: ${error}`);
    return { request_id: request.request_id, status: 'error', errors: [error] };
  }
}

/**
 * Handle the 'position_rows' action - write the compiler's rows for each
 * call, member access, comparison and identifier of the given files
 * (carrick#2405). Asked of a process of its own: it releases each owner's
 * program once its files are walked, so nothing built before it is kept.
 */
function handlePositionRows(
  request: SidecarRequest & { action: 'position_rows' }
): PositionRowsResponse {
  try {
    const loader = initializedLoader();
    // Built over programs the walk releases; nothing may read them after it.
    session.components = new Map();
    let done = 0;
    const report = atMostEvery(PROGRESS_INTERVAL_MS, () =>
      writeProgress(request.request_id, 'positions', `${done} of ${request.files.length}`)
    );
    const result = writePositionRows({
      loader,
      files: request.files,
      scanRoot: request.scan_root,
      outPath: request.out_path,
      service: request.service,
      onFile: () => {
        done += 1;
        report();
      },
    });
    log(
      `Position rows: ${result.rows} row(s) over ${result.files} file(s), ` +
        `${result.bytes} byte(s), load ${result.load_ms}ms, walk ${result.walk_ms}ms`
    );
    return { request_id: request.request_id, status: 'success', result };
  } catch (err) {
    const error = err instanceof Error ? err.message : String(err);
    logError(`Position rows failed: ${error}`);
    return { request_id: request.request_id, status: 'error', errors: [error] };
  }
}

/**
 * Handle the 'health' action - report initialization status
 */
function handleHealth(request: SidecarRequest & { action: 'health' }): HealthResponse {
  const isReady = session.projectLoader?.isInitialized() ?? false;

  return {
    request_id: request.request_id,
    status: isReady ? 'ready' : 'not_ready',
    init_time_ms: session.initTimeMs ?? undefined,
  };
}

/**
 * Handle the 'shutdown' action - exit gracefully
 */
function handleShutdown(request: SidecarRequest & { action: 'shutdown' }): ShutdownResponse {
  log('Shutdown requested');

  // Schedule exit after response is sent
  setImmediate(() => {
    log('Exiting');
    process.exit(0);
  });

  return {
    request_id: request.request_id,
    status: 'success',
  };
}

// ===========================================================================
// Request Router
// ===========================================================================

/**
 * Route a request to the appropriate handler
 */
function handleRequest(request: SidecarRequest): SidecarResponse {
  switch (request.action) {
    case 'init':
      return handleInit(request);
    case 'bundle':
      return handleBundle(request);
    case 'capture_v2':
      return handleCaptureV2(request);
    case 'infer':
      return handleInfer(request);
    case 'resolve_definitions':
      return handleResolveDefinitions(request);
    case 'retype_check':
      return handleRetypeCheck(request);
    case 'verify_client_semantics':
      return handleVerifyClientSemantics(request);
    case 'verify_library_claims':
      return handleVerifyLibraryClaims(request);
    case 'list_library_surface':
      return handleListLibrarySurface(request);
    case 'list_program_files':
      return handleListProgramFiles(request);
    case 'add_program_files':
      return handleAddProgramFiles(request);
    case 'position_rows':
      return handlePositionRows(request);
    case 'implementations':
      return handleImplementations(request);
    case 'health':
      return handleHealth(request);
    case 'shutdown':
      return handleShutdown(request);
    case 'hello':
    case 'status':
    case 'stop':
      return {
        request_id: request.request_id,
        status: 'error',
        errors: [`${request.action} is answered in server mode only (--serve)`],
      };
    default:
      // TypeScript should catch this, but just in case
      return {
        request_id: (request as { request_id: string }).request_id || 'unknown',
        status: 'error',
        errors: [`Unknown action: ${(request as { action: string }).action}`],
      } as ErrorResponse;
  }
}

// ===========================================================================
// Response Writer
// ===========================================================================

/** Where the frames of the request being handled go. */
type Sink = (frame: string) => void;

const STDOUT: Sink = (frame) => {
  process.stdout.write(frame);
};

/**
 * The frames of the request being handled: stdout, or in server mode the
 * connection that sent it. A request that answers later (`check_v2`) keeps the
 * one it started with.
 */
let sink: Sink = STDOUT;

/**
 * Write a JSON response to stdout
 */
function writeResponse(response: SidecarResponse, out: Sink = sink): void {
  out(JSON.stringify(response) + '\n');
}

/**
 * Write a non-terminal progress frame. Distinct `status: 'progress'` so
 * clients skip it and wait for the terminal success/error frame; the scanner
 * restarts its deadline for the request on each one (carrick#1914).
 *
 * A synchronous handler calls this between units of its work, with the event
 * loop blocked. The frame still leaves at once: a write this small to a pipe
 * with room in it completes inside the call, and the reader on the other end
 * drains the pipe on its own thread, so there is room. It is written through
 * the same stream as every answer, so a frame can never land inside one.
 */
function writeProgress(requestId: string, phase: string, message: string, out: Sink = sink): void {
  out(JSON.stringify({ request_id: requestId, status: 'progress', phase, message }) + '\n');
}

/**
 * Write an error response for an invalid request
 */
function writeErrorResponse(requestId: string, error: string): void {
  const response: ErrorResponse = {
    request_id: requestId,
    status: 'error',
    errors: [error],
  };
  writeResponse(response);
}

// ===========================================================================
// Logging (to stderr only)
// ===========================================================================

function log(message: string): void {
  console.error(`[sidecar] ${message}`);
}

function logError(message: string): void {
  console.error(`[sidecar:error] ${message}`);
}

// ===========================================================================
// Main Entry Point
// ===========================================================================

/**
 * The request a line holds, or null when it is empty or invalid (an invalid
 * one is answered with an error frame through the current sink).
 */
function requestOf(line: string): SidecarRequest | null {
  // Skip empty lines
  const trimmed = line.trim();
  if (!trimmed) return null;

  // Parse JSON
  let json: unknown;
  try {
    json = JSON.parse(trimmed);
  } catch (err) {
    logError(`Invalid JSON: ${trimmed}`);
    writeErrorResponse('unknown', `Invalid JSON: ${err instanceof Error ? err.message : String(err)}`);
    return null;
  }

  // Validate request
  const parseResult = parseRequest(json);
  if (!parseResult.success) {
    logError(`Invalid request: ${parseResult.error}`);
    writeErrorResponse(
      (json as { request_id?: string })?.request_id || 'unknown',
      parseResult.error
    );
    return null;
  }
  return parseResult.request;
}

/**
 * Answer one request through the current sink. `check_v2` runs async
 * (spawned pnpm install + tsc) and writes its own progress and terminal
 * frames; the promise settles once it has.
 */
function dispatch(request: SidecarRequest): Promise<void> {
  if (request.action === 'check_v2') return handleCheckV2Async(request);
  writeResponse(handleRequest(request));
  return Promise.resolve();
}

/** Process a single line of stdin. */
function processLine(line: string): void {
  const request = requestOf(line);
  // Over stdio a check runs beside the requests after it, as before.
  if (request) void dispatch(request);
}

// ===========================================================================
// Server mode (carrick#2879)
// ===========================================================================

/** Sessions a daemon holds, by what their `init` named. */
const heldSessions = new Map<string, Session>();

/**
 * Server mode: what each service's last capture kept (carrick#2882), by the
 * fields that scope a capture. Null over stdio, where every capture is cold.
 */
let captureHolds: Map<string, { hold: CaptureHold; repo_root: string; service_name: string; evictions: number }> | null = null;

/** The hold for `request`'s service, made on its first capture; undefined over stdio. */
function captureHoldFor(request: SidecarRequest & { action: 'capture_v2' }): CaptureHold | undefined {
  if (!captureHolds) return undefined;
  const key = JSON.stringify([
    path.resolve(request.repo_root),
    request.tsconfig_path ?? null,
    request.scan_root ?? null,
    request.service_name,
  ]);
  let held = captureHolds.get(key);
  if (!held) {
    held = { hold: new CaptureHold(emitReach()), repo_root: path.resolve(request.repo_root), service_name: request.service_name, evictions: 0 };
    captureHolds.set(key, held);
  }
  held.hold.lastUsed = performance.now();
  return held.hold;
}

/**
 * carrick#2882: the files whose declarations an edit can change, read from
 * the import graph of the emit program the kept output came from, and the
 * graph carried to the next one (as `graphFor` carries a held project's).
 */
function emitReach(): EmitReach {
  let carried: { graph: ImportGraph; program: unknown } | undefined;
  return (before, after, changes) => {
    const graph = carried?.program === before ? carried.graph : importGraphOf(before as never);
    const reached = affectedFiles(graph, changes);
    if (reached === 'all' || reached.exportsMoved) {
      carried = undefined;
      return reached === 'all' ? 'all' : reached.files;
    }
    readAgain(graph, after as never, changes.map((change) => change.file));
    carried = { graph, program: after };
    return reached.files;
  };
}

/** The key of the service an `init` names: every field that scopes its project. */
function sessionKey(request: SidecarRequest & { action: 'init' }): string {
  return JSON.stringify([
    path.resolve(request.repo_root),
    request.tsconfig_path ?? null,
    request.scan_root ?? null,
    request.tsconfig_snapshot ?? null,
    request.pinned_dependencies ?? null,
  ]);
}

/**
 * In server mode, `init` selects the held session for its service, or builds
 * one: a second client of the same service reads the project the first one
 * built. Over stdio, `init` re-scopes the one session instead.
 */
function initHeld(request: SidecarRequest & { action: 'init' }): InitResponse {
  const key = sessionKey(request);
  const held = heldSessions.get(key);
  if (held?.projectLoader?.isInitialized()) {
    session = held;
    return { request_id: request.request_id, status: 'ready', init_time_ms: held.initTimeMs ?? undefined };
  }
  session = held && !held.projectLoader ? held : newSession();
  session.scope = { repo_root: path.resolve(request.repo_root), scan_root: request.scan_root ?? null };
  session.init = request;
  session.items = new Map();
  const response = handleInit(request);
  if (response.status === 'ready') heldSessions.set(key, session);
  return response;
}

function statusOf(server: TypedServer, buildId: string, cliVersion: string | null): StatusResponse['daemon'] {
  const memory = process.memoryUsage();
  return {
    pid: process.pid,
    build_id: buildId,
    cli_version: cliVersion,
    socket: server.socketPath,
    started_at: server.startedAt.toISOString(),
    uptime_ms: Date.now() - server.startedAt.getTime(),
    idle_ms: server.idleMs(),
    services: [...heldSessions.values()].map((held) => ({
      repo_root: held.scope?.repo_root ?? '',
      scan_root: held.scope?.scan_root ?? null,
      built: held.components.size > 0,
      freshness: { ...held.freshness },
      items: { held: held.items?.size ?? 0, ...held.answers },
      evictions: held.evictions,
    })),
    captures: [...(captureHolds?.values() ?? [])].map((held) => ({
      repo_root: held.repo_root,
      service_name: held.service_name,
      ...held.hold.report(),
      evictions: held.evictions,
    })),
    heap_cap_mb: heapCapMb(),
    judge: { ...(keptJudge?.uses ?? { checks: 0, installs_skipped: 0 }) },
    ...(verifyEnabled() ? { verify: { ...verifyCounts } } : {}),
    heap_used_mb: Math.round(memory.heapUsed / 1048576),
    rss_mb: Math.round(memory.rss / 1048576),
  };
}

/** Answer one line from a connection, with its frames going back to it. */
async function serveLine(
  server: TypedServer,
  buildId: string,
  cliVersion: string | null,
  conn: Connection,
  line: string
): Promise<void> {
  sink = (frame) => conn.write(frame);
  try {
    const request = requestOf(line);
    if (!request) return;
    if (!conn.greeted) {
      if (request.action !== 'hello' || request.build_id !== buildId) {
        const saw = request.action === 'hello' ? `build ${request.build_id}` : `'${request.action}' first`;
        await conn.flush(
          JSON.stringify({
            request_id: request.request_id,
            status: 'error',
            errors: [`build_mismatch: this daemon is build ${buildId}; a connection must open with hello naming it, saw ${saw}`],
          }) + '\n'
        );
        conn.close();
        return;
      }
      conn.greeted = true;
      writeResponse({ request_id: request.request_id, status: 'success', build_id: buildId, pid: process.pid });
      return;
    }
    switch (request.action) {
      case 'hello':
        writeResponse({ request_id: request.request_id, status: 'success', build_id: buildId, pid: process.pid });
        return;
      case 'status':
        writeResponse({ request_id: request.request_id, status: 'success', daemon: statusOf(server, buildId, cliVersion) });
        return;
      case 'stop':
        await conn.flush(JSON.stringify({ request_id: request.request_id, status: 'success' }) + '\n');
        server.stop('stop requested');
        return;
      case 'shutdown':
        // The client is done; the daemon is not.
        await conn.flush(JSON.stringify({ request_id: request.request_id, status: 'success' }) + '\n');
        conn.close();
        return;
      case 'init':
        writeResponse(initHeld(request));
        conn.slot = session;
        session.lastUsed = performance.now();
        return;
      default: {
        const held = conn.slot as Session | undefined;
        session = held ?? newSession();
        if (held) held.lastUsed = performance.now();
        const answer = async (): Promise<void> => {
          if (held && READS_HELD_PROJECT.has(request.action)) writeResponse(answerFresh(request));
          else await dispatch(request);
        };
        if (verifyEnabled()) await answerVerified(conn, line, held?.init ?? null, answer);
        else await answer();
      }
    }
  } finally {
    sink = STDOUT;
    session = newSession();
    evictOverCap(conn.slot as Session | undefined);
  }
}

/** Verify mode (carrick#2886): what it has seen, and the cold runner it asks. */
const verifyCounts = newVerifyCounts();
let coldRunner: ColdRunner | null = null;

/**
 * Verify mode: run `answer` with its frames held back, answer the same request
 * cold, and write the terminal frame that `verifyAnswer` picks. Progress frames
 * go out as they are written.
 */
async function answerVerified(
  conn: Connection,
  line: string,
  init: object | null,
  answer: () => Promise<void>
): Promise<void> {
  const terminal: string[] = [];
  sink = (frame) => {
    if ((JSON.parse(frame) as { status?: string }).status === 'progress') conn.write(frame);
    else terminal.push(frame);
  };
  await answer();
  sink = (frame) => conn.write(frame);
  if (terminal.length !== 1) {
    log(`verify: ${terminal.length} terminal frames, not compared`);
    for (const frame of terminal) conn.write(frame);
    return;
  }
  coldRunner ??= spawnColdRunner(fileURLToPath(import.meta.url));
  const outcome = await verifyAnswer(terminal[0], init, line, coldRunner, verifyCounts, log);
  conn.write(outcome.frame);
}

/** The heap a daemon keeps its held services under (carrick#2881). */
const DEFAULT_HEAP_CAP_MB = 3072;

function heapCapMb(): number {
  const cap = Number(process.env.CARRICK_TYPED_HEAP_CAP_MB);
  return Number.isFinite(cap) && cap > 0 ? cap : DEFAULT_HEAP_CAP_MB;
}

/**
 * carrick#2881: while the heap is over its cap, release the held service that
 * answered least recently, never `current` (the one that just answered). An
 * evicted service keeps its `init` and is built again, cold, by its next
 * request. Without a collector to call (`--expose-gc`), the heap only shrinks
 * once the next collection runs, so one service is released per request.
 */
function evictOverCap(current: Session | undefined): void {
  const gc = (globalThis as { gc?: () => void }).gc;
  for (;;) {
    const heapMb = process.memoryUsage().heapUsed / 1048576;
    if (heapMb <= heapCapMb()) return;
    // A held capture is released as a service is (carrick#2882), never the one that just answered.
    const captures = [...(captureHolds?.values() ?? [])].filter((held) => held.hold.emit || held.hold.selfCheck);
    const newest = Math.max(...captures.map((held) => held.hold.lastUsed));
    const victim = [
      ...[...heldSessions.values()]
        .filter((held) => held !== current && held.projectLoader !== null)
        .map((held) => ({ lastUsed: held.lastUsed, label: held.scope?.repo_root ?? 'a held service', release: () => releaseSession(held) })),
      ...captures
        .filter((held) => held.hold.lastUsed !== newest)
        .map((held) => ({
          lastUsed: held.hold.lastUsed,
          label: `the capture of ${held.service_name}`,
          release: () => {
            held.hold = new CaptureHold(emitReach());
            held.evictions += 1;
          },
        })),
    ].sort((a, b) => a.lastUsed - b.lastUsed)[0];
    if (!victim) return;
    log(`Heap ${Math.round(heapMb)} MB is over its cap of ${heapCapMb()} MB: releasing ${victim.label} (carrick#2881)`);
    victim.release();
    if (!gc) return;
    gc();
  }
}

function releaseSession(victim: Session): void {
  victim.projectLoader = null;
  victim.components = new Map();
  victim.items = victim.items && new Map();
  victim.held = null;
  victim.graphs = new Map();
  victim.evictions += 1;
}

/**
 * The actions that read the held project (carrick#2880). The rest build what
 * they read from the request (`capture_v2`, `check_v2`, `resolve_definitions`)
 * or read nothing.
 */
const READS_HELD_PROJECT = new Set<SidecarRequest['action']>([
  'bundle',
  'infer',
  'retype_check',
  'verify_client_semantics',
  'verify_library_claims',
  'list_library_surface',
  'list_program_files',
  'add_program_files',
  'position_rows',
  'implementations',
]);

/** How many times a request is answered again when files change while it runs. */
const MAX_ANSWERS = 3;

/**
 * Bring the current session up to date with the disk: nothing, a refresh of
 * the changed files, or a rebuild from its `init` (`held-freshness.ts`).
 */
function freshen(): void {
  if (session.init && session.projectLoader === null) {
    // Evicted to keep the heap under its cap: built again as a cold process builds it.
    log('rebuilding an evicted service (carrick#2881)');
    handleInit(session.init);
    session.items = new Map();
    session.held = null;
  }
  if (!session.projectLoader?.isInitialized() || !session.init) return;
  session.held ??= heldFilesOf(session);
  const found = session.held.sweep();
  if (found.outcome === 'refresh') {
    log(`refreshing ${found.files.length} changed file(s)`);
    const before = session.items ? textsHeld(found.files) : null;
    session.held.refresh(found.files);
    // Everything built over the programs reads them again.
    session.components = new Map();
    session.freshness.refreshes += 1;
    if (session.items && before) {
      const reached = reachedBy(before);
      const dropped = dropReached(session.items, reached);
      log(
        `The edit reaches ${reached === 'all' ? 'every file' : `${reached.files.size} file(s)`}: ${dropped} kept infer item(s) dropped (carrick#2881)`
      );
    }
  } else if (found.outcome === 'drop') {
    log(`rebuilding the service: ${found.reason}`);
    session.freshness.drops += 1;
    session.items?.clear();
    session.graphs = new Map();
    handleInit(session.init);
    // Watched from now on, so a change while the next request runs is found.
    session.held = session.projectLoader?.isInitialized() ? heldFilesOf(session) : null;
    session.held?.sweep();
  }
}

/**
 * carrick#2881: each held program, with the text it holds of each of `files`
 * and its import graph, read before the files are read again.
 */
function textsHeld(files: string[]): Array<{ project: Project; graph: ImportGraph; before: Map<string, string> }> {
  return initializedLoader()
    .builtProjects()
    .map((project) => {
      const program = project.getProgram().compilerObject;
      const before = new Map<string, string>();
      for (const file of files) {
        const text = program.getSourceFile(file)?.text;
        if (text !== undefined) before.set(file, text);
      }
      return { project, graph: graphFor(project), before };
    });
}

/**
 * carrick#2881: what the refreshed files reach, in every held program. A
 * carried graph reads the changed files again on its next use, and is read
 * whole again after an edit that moved an export.
 */
function reachedBy(held: Array<{ project: Project; graph: ImportGraph; before: Map<string, string> }>): Reached {
  const files = new Set<string>();
  let exportsMoved = false;
  let everything = false;
  for (const { project, graph, before } of held) {
    const changes: FileChange[] = [];
    for (const [file, text] of before) {
      changes.push({ file, before: text, after: project.getSourceFile(file)?.getFullText() ?? '' });
    }
    const reached = affectedFiles(graph, changes);
    const carried = session.graphs.get(project);
    if (reached === 'all' || reached.exportsMoved) session.graphs.delete(project);
    else for (const file of before.keys()) carried?.stale.add(file);
    if (reached === 'all') {
      everything = true;
      continue;
    }
    // Kept items name their file by its real path (`realFileOf`).
    for (const file of reached.files) files.add(realFileOf(file));
    exportsMoved ||= reached.exportsMoved;
  }
  return everything ? 'all' : { files, exportsMoved };
}

/** carrick#2881: `project`'s import graph as its program stands, carried across edits. */
function graphFor(project: Project): ImportGraph {
  const program = project.getProgram().compilerObject;
  const carried = session.graphs.get(project);
  if (!carried) {
    const graph = importGraphOf(program);
    session.graphs.set(project, { graph, stale: new Set() });
    return graph;
  }
  if (carried.stale.size > 0) {
    readAgain(carried.graph, program, carried.stale);
    carried.stale.clear();
  }
  return carried.graph;
}

/** The files `held`'s current loader builds from, ready to sweep. */
function heldFilesOf(held: Session): HeldFiles {
  const loader = held.projectLoader!;
  const scope = held.scope;
  return new HeldFiles({
    projects: () => loader.builtProjects(),
    refreshable: () => loader.buildsFromTsconfigs(),
    roots: () => [...new Set([scope?.repo_root, scope?.scan_root].filter((r): r is string => !!r))],
  });
}

/**
 * Answer a request that reads the held project from programs equal to the
 * disk. Files that change while it runs make the answer stale: it is dropped
 * and the request answered again, at most {@link MAX_ANSWERS} times.
 */
function answerFresh(request: SidecarRequest): SidecarResponse {
  const answer = answerWhileStable(
    () => handleRequest(request),
    freshen,
    () => {
      const stable = !session.held || session.held.sweep().outcome === 'none';
      if (!stable) log('files changed while the request ran; answering it again');
      return stable;
    },
    MAX_ANSWERS
  );
  return (
    answer ?? {
      request_id: request.request_id,
      status: 'error',
      errors: [`files kept changing while the request ran (${MAX_ANSWERS} answers dropped)`],
    }
  );
}

/** `--serve <workspace> [--cli-version <v>]`: run as the workspace's daemon. */
async function serve(workspace: string, cliVersion: string | null): Promise<void> {
  const buildId = buildIdOf(DIST_SRC);
  captureHolds = new Map();
  const idle = Number(process.env.CARRICK_TYPED_IDLE_MS);
  const server: TypedServer = new TypedServer({
    workspace: path.resolve(workspace),
    buildId,
    cliVersion,
    idleMs: Number.isFinite(idle) && idle > 0 ? idle : DEFAULT_IDLE_MS,
    handleLine: (conn, line) => serveLine(server, buildId, cliVersion, conn, line),
    log,
  });
  const outcome = await server.start();
  if (outcome === 'already_running') {
    log(`a daemon already serves ${workspace}`);
    process.exit(EXIT_ALREADY_RUNNING);
  }
  const judge = new KeptJudge();
  keptJudge = judge;
  process.on('exit', () => judge.discard());
  const stopOn = (signal: string) => () => server.stop(`${signal} received`);
  process.on('SIGINT', stopOn('SIGINT'));
  process.on('SIGTERM', stopOn('SIGTERM'));
}

/** How often the sidecar looks for the process that started it. */
const PARENT_CHECK_MS = 1000;

/**
 * Exit once the process that started this one is gone (carrick#2029).
 *
 * Its death closes stdin, which ends this process, unless another process
 * still holds the write end. Then the only sign is that this process has a
 * new parent: whatever adopts it (init, or a subreaper on Linux) has a pid of
 * its own. So the parent at startup is kept and compared, never 1. A process
 * already adopted at startup has no parent to watch, and stdin alone decides.
 *
 * The timer is unref'd: it never keeps an otherwise finished process alive.
 * A handler that blocks the event loop holds it off until it returns.
 */
function exitWhenParentIsGone(): void {
  const startedBy = process.ppid;
  if (startedBy <= 1) return;
  setInterval(() => {
    if (process.ppid !== startedBy) {
      log(`parent ${startedBy} is gone, exiting`);
      process.exit(0);
    }
  }, PARENT_CHECK_MS).unref();
}

/** This build's compiled sources: what its build id hashes. */
const DIST_SRC = path.dirname(fileURLToPath(import.meta.url));

/** The value after `flag` on the command line, or null. */
function argValue(flag: string): string | null {
  const at = process.argv.indexOf(flag);
  return at === -1 ? null : (process.argv[at + 1] ?? null);
}

/**
 * Start the message loop
 */
function main(): void {
  if (process.argv.includes('--build-id')) {
    process.stdout.write(buildIdOf(DIST_SRC) + '\n');
    return;
  }
  const workspace = argValue('--serve');
  if (workspace !== null) {
    log(`Process started in server mode`);
    installFatalHandlers();
    void serve(workspace, argValue('--cli-version'));
    return;
  }

  log('Process started');

  const rl = readline.createInterface({
    input: process.stdin,
    output: process.stdout,
    terminal: false,
  });

  rl.on('line', processLine);

  rl.on('close', () => {
    log('stdin closed, exiting');
    process.exit(0);
  });

  exitWhenParentIsGone();

  // Handle process signals
  process.on('SIGINT', () => {
    log('SIGINT received, exiting');
    process.exit(0);
  });

  process.on('SIGTERM', () => {
    log('SIGTERM received, exiting');
    process.exit(0);
  });

  installFatalHandlers();
}

function installFatalHandlers(): void {
  // Fail fast on uncaught errors. Continuing after one means serving later
  // requests from a possibly corrupted state and returning garbage types
  // with a success status; exiting surfaces as ProcessDied on the Rust side,
  // which degrades cleanly (scan continues, type_extraction_status records
  // the loss).
  process.on('uncaughtException', (err) => {
    logError(`Uncaught exception (exiting): ${err.message}`);
    logError(err.stack || '');
    process.exit(1);
  });

  process.on('unhandledRejection', (reason) => {
    // This log is the last diagnostic before exit, so it must be actionable:
    // a bare interpolation renders non-Error reasons as [object Object].
    let detail: string;
    if (reason instanceof Error) {
      detail = reason.stack || reason.message;
    } else {
      try {
        detail = JSON.stringify(reason) ?? String(reason);
      } catch {
        detail = String(reason);
      }
    }
    logError(`Unhandled rejection (exiting): ${detail}`);
    process.exit(1);
  });
}

// Start the sidecar
main();
