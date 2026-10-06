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

import * as path from 'node:path';
import * as readline from 'node:readline';
import { parseRequest } from './validators.js';
import { ProjectLoader } from './project-loader.js';
import { TypeBundler } from './bundler.js';
import { TypeInferrer } from './type-inferrer.js';
import { DefinitionResolver } from './definition-resolver.js';
import {
  captureStub,
  findDisqualifyingTopTypes,
  jsonWireDeclarations,
  runCheck,
} from './capture/index.js';
import { Retyper, type TopTypeWalk } from './retype.js';
import { LibraryClaimsVerifier, httpCheck } from './library-claims.js';
import { PROGRESS_INTERVAL_MS, atMostEvery } from './progress.js';
import { mergeInferTimings } from './infer-timing.js';
import { addProgramFiles, listProgramFiles } from './program-files.js';
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
  ErrorResponse,
} from './types.js';

// ===========================================================================
// Module-level state
// ===========================================================================

let projectLoader: ProjectLoader | null = null;
let initTimeMs: number | null = null;

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
}

/**
 * Components by project key (`ProjectLoader.projectKeyFor`): `''` is the
 * default project, any other key the program of a project that owns some of
 * the service's files (carrick#1604).
 */
let components = new Map<string, ProjectComponents>();

/**
 * Reads a capture stub's own declaration tree and nothing of the init'd
 * project, so it is not one of the project's components: taking it from them
 * would build the service's whole program to answer about the stub
 * (carrick#1927).
 */
const definitionResolver = new DefinitionResolver();

/** The init'd project loader, or the error every project-backed action gives without one. */
function initializedLoader(): ProjectLoader {
  if (!projectLoader?.isInitialized()) {
    throw new Error('Sidecar not initialized. Call init first.');
  }
  return projectLoader;
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
  let built = components.get(key);
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
      claimsVerifier: new LibraryClaimsVerifier(project),
    };
    components.set(key, built);
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
    const key = projectLoader?.projectKeyFor(fileOf(item)) ?? '';
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
    components = new Map();
    projectLoader = new ProjectLoader({
      repoRoot: request.repo_root,
      tsconfigPath: request.tsconfig_path,
      scanRoot: request.scan_root,
      tsconfigSnapshot: request.tsconfig_snapshot,
      pinnedDependencies: request.pinned_dependencies,
    });

    const result = projectLoader.load();

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

    initTimeMs = result.initTimeMs || Math.round(performance.now() - startTime);

    log(`Initialization complete in ${initTimeMs}ms`);

    return {
      request_id: request.request_id,
      status: 'ready',
      init_time_ms: initTimeMs,
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
    const result = captureStub({
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
    });
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
async function handleCheckV2Async(
  request: SidecarRequest & { action: 'check_v2' }
): Promise<void> {
  let phase = 'assembling';
  const keepalive = setInterval(() => {
    writeProgress(request.request_id, phase, `check ${phase}`);
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
    writeResponse(response);
  } catch (err) {
    clearInterval(keepalive);
    const error = err instanceof Error ? err.message : String(err);
    logError(`check_v2 failed: ${error}`);
    writeResponse({
      request_id: request.request_id,
      status: 'error',
      errors: [error],
    });
  }
}

/**
 * Handle the 'infer' action - infer implicit types
 */
function handleInfer(request: SidecarRequest & { action: 'infer' }): InferResponse {
  try {
    log(`Inferring ${request.requests.length} type(s)`);

    // One count for the request, whichever programs its items span.
    let done = 0;
    const report = atMostEvery(PROGRESS_INTERVAL_MS, () =>
      writeProgress(request.request_id, 'infer', `${done} of ${request.requests.length}`)
    );
    const results = [...byProject(request.requests, (item) => item.file_path)].map(
      ([key, items]) =>
        projectComponents(key).typeInferrer.infer(items, request.extraction_config, () => {
          done += 1;
          report();
        })
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

    return {
      request_id: request.request_id,
      status: result.success ? 'success' : 'error',
      inferred_types: result.inferred_types,
      // Beside the answers and never in them: what the requests cost, for
      // the caller's log (carrick#1985).
      infer_timing: mergeInferTimings(results.map((r) => r.timing)),
      errors: result.errors,
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
    const fromDir = path.resolve(projectLoader!.getRepoRoot(), request.from_dir);
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
    const fromDir = path.resolve(projectLoader!.getRepoRoot(), request.from_dir);
    log(`Verifying ${request.checks.length} library claim(s) from ${fromDir}`);
    const { semantics, modules } = claimsVerifier.run(fromDir, request.checks, request.budget_ms ?? SEMANTICS_BUDGET_MS);
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
    const fromDir = path.resolve(projectLoader!.getRepoRoot(), request.from_dir);
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
 * Handle the 'health' action - report initialization status
 */
function handleHealth(request: SidecarRequest & { action: 'health' }): HealthResponse {
  const isReady = projectLoader?.isInitialized() ?? false;

  return {
    request_id: request.request_id,
    status: isReady ? 'ready' : 'not_ready',
    init_time_ms: initTimeMs ?? undefined,
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
    case 'health':
      return handleHealth(request);
    case 'shutdown':
      return handleShutdown(request);
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

/**
 * Write a JSON response to stdout
 */
function writeResponse(response: SidecarResponse): void {
  const json = JSON.stringify(response);
  process.stdout.write(json + '\n');
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
function writeProgress(requestId: string, phase: string, message: string): void {
  process.stdout.write(
    JSON.stringify({ request_id: requestId, status: 'progress', phase, message }) + '\n'
  );
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
 * Process a single line of input
 */
function processLine(line: string): void {
  // Skip empty lines
  const trimmed = line.trim();
  if (!trimmed) return;

  // Parse JSON
  let json: unknown;
  try {
    json = JSON.parse(trimmed);
  } catch (err) {
    logError(`Invalid JSON: ${trimmed}`);
    writeErrorResponse('unknown', `Invalid JSON: ${err instanceof Error ? err.message : String(err)}`);
    return;
  }

  // Validate request
  const parseResult = parseRequest(json);
  if (!parseResult.success) {
    logError(`Invalid request: ${parseResult.error}`);
    writeErrorResponse(
      (json as { request_id?: string })?.request_id || 'unknown',
      parseResult.error
    );
    return;
  }

  // check_v2 runs async (spawned pnpm install + tsc) and writes its own
  // progress + terminal frames; hand off and return.
  if (parseResult.request.action === 'check_v2') {
    void handleCheckV2Async(parseResult.request);
    return;
  }

  // Handle request
  const response = handleRequest(parseResult.request);
  writeResponse(response);
}

/**
 * Start the message loop
 */
function main(): void {
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

  // Handle process signals
  process.on('SIGINT', () => {
    log('SIGINT received, exiting');
    process.exit(0);
  });

  process.on('SIGTERM', () => {
    log('SIGTERM received, exiting');
    process.exit(0);
  });

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
