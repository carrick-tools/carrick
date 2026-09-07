/**
 * Main entry point for the type-sidecar
 *
 * This module implements a message loop that:
 * 1. Listens on stdin for JSON requests
 * 2. Processes each request (init, bundle, emit_surface, infer, build_workspace, check_compatibility, health, shutdown)
 * 3. Writes JSON responses to stdout
 *
 * IMPORTANT:
 * - stdout is ONLY for JSON responses
 * - stderr is for logging
 * - Process stays alive between requests (warm standby)
 */

import * as readline from 'node:readline';
import { parseRequest } from './validators.js';
import { ProjectLoader } from './project-loader.js';
import { TypeBundler, SurfaceEmitter } from './bundler.js';
import { TypeInferrer } from './type-inferrer.js';
import { MonorepoBuilder } from './monorepo-builder.js';
import { DefinitionResolver } from './definition-resolver.js';
import { captureStub, runCheck } from './capture/index.js';
import type {
  SidecarRequest,
  SidecarResponse,
  InitResponse,
  BundleResponse,
  EmitSurfaceResponse,
  CaptureV2Response,
  CheckV2Response,
  InferResponse,
  BuildWorkspaceResponse,
  CheckCompatibilityResponse,
  ResolveDefinitionsResponse,
  HealthResponse,
  ShutdownResponse,
  ErrorResponse,
} from './types.js';

// ===========================================================================
// Module-level state
// ===========================================================================

let projectLoader: ProjectLoader | null = null;
let monorepoBuilder: MonorepoBuilder | null = null;
let initTimeMs: number | null = null;

/**
 * The components that read the init'd ts-morph project. Built together with
 * the project on first use and dropped on re-init, so re-scoping the sidecar
 * to another service can never serve the previous service's program.
 */
interface ProjectComponents {
  typeBundler: TypeBundler;
  surfaceEmitter: SurfaceEmitter;
  typeInferrer: TypeInferrer;
  definitionResolver: DefinitionResolver;
}

let components: ProjectComponents | null = null;

/**
 * Get the project-backed components, building the project if this is the
 * first request that needs it.
 *
 * @throws if init has not run, or if the project cannot be built
 */
function projectComponents(): ProjectComponents {
  if (!projectLoader?.isInitialized()) {
    throw new Error('Sidecar not initialized. Call init first.');
  }
  if (!components) {
    const project = projectLoader.getProject();
    const repoRoot = projectLoader.getRepoRoot();
    components = {
      typeBundler: new TypeBundler({ project, repoRoot }),
      surfaceEmitter: new SurfaceEmitter({ project, repoRoot }),
      typeInferrer: new TypeInferrer({ project }),
      definitionResolver: new DefinitionResolver({ project }),
    };
  }
  return components;
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
    components = null;
    projectLoader = new ProjectLoader({
      repoRoot: request.repo_root,
      tsconfigPath: request.tsconfig_path,
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

    // Initialize monorepo builder (doesn't need project)
    monorepoBuilder = new MonorepoBuilder();

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

    const result = projectComponents().typeBundler.bundle(request.symbols);

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
 * Handle the 'emit_surface' action - emit a surface .d.ts with rewritten specifiers
 */
function handleEmitSurface(request: SidecarRequest & { action: 'emit_surface' }): EmitSurfaceResponse {
  try {
    log(`Emitting surface for repo '${request.repo_name}' with ${request.payloads.length} payload(s)`);

    const result = projectComponents().surfaceEmitter.emit(
      request.repo_name,
      request.payloads,
      request.output_path
    );

    if (!result.success) {
      return {
        request_id: request.request_id,
        status: 'error',
        errors: result.errors,
      };
    }

    return {
      request_id: request.request_id,
      status: 'success',
      output_path: result.output_path,
      surface_content: result.surface_content,
      manifest: result.manifest,
    };
  } catch (err) {
    const error = err instanceof Error ? err.message : String(err);
    logError(`Surface emission failed: ${error}`);

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
    const result = captureStub({
      repoRoot: request.repo_root,
      serviceName: request.service_name,
      anchors: request.anchors,
      outDir: request.out_dir,
      tsconfigPath: request.tsconfig_path,
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

    const result = projectComponents().typeInferrer.infer(
      request.requests,
      request.extraction_config
    );

    return {
      request_id: request.request_id,
      status: result.success ? 'success' : 'error',
      inferred_types: result.inferred_types,
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
 * Handle the 'build_workspace' action - build synthetic monorepo workspace
 */
function handleBuildWorkspace(request: SidecarRequest & { action: 'build_workspace' }): BuildWorkspaceResponse {
  // MonorepoBuilder doesn't require project initialization
  if (!monorepoBuilder) {
    monorepoBuilder = new MonorepoBuilder();
  }

  try {
    log(`Building synthetic workspace with ${request.repos.length} repo(s)`);

    const result = monorepoBuilder.build(request.repos, request.workspace_root);

    if (!result.success) {
      return {
        request_id: request.request_id,
        status: 'error',
        errors: result.errors,
      };
    }

    return {
      request_id: request.request_id,
      status: 'success',
      workspace_path: result.workspace_path,
      stub_packages: result.stub_packages,
      checker_path: result.checker_path,
    };
  } catch (err) {
    const error = err instanceof Error ? err.message : String(err);
    logError(`Workspace build failed: ${error}`);

    return {
      request_id: request.request_id,
      status: 'error',
      errors: [error],
    };
  }
}

/**
 * Handle the 'check_compatibility' action - run type compatibility checks
 */
function handleCheckCompatibility(request: SidecarRequest & { action: 'check_compatibility' }): CheckCompatibilityResponse {
  if (!monorepoBuilder) {
    monorepoBuilder = new MonorepoBuilder();
  }

  try {
    log(`Running compatibility checks: ${request.checks.length} check(s)`);

    const result = monorepoBuilder.checkCompatibility(
      request.workspace_root,
      request.checks
    );

    if (!result.success) {
      return {
        request_id: request.request_id,
        status: 'error',
        errors: result.errors,
      };
    }

    return {
      request_id: request.request_id,
      status: 'success',
      results: result.results,
      diagnostics: result.diagnostics,
    };
  } catch (err) {
    const error = err instanceof Error ? err.message : String(err);
    logError(`Compatibility check failed: ${error}`);

    return {
      request_id: request.request_id,
      status: 'error',
      errors: [error],
    };
  }
}

/**
 * Handle the 'resolve_definitions' action - resolve surface aliases from a
 * v2 capture stub package's declaration tree.
 */
function handleResolveDefinitions(
  request: SidecarRequest & { action: 'resolve_definitions' },
): ResolveDefinitionsResponse {
  try {
    log(`Resolving ${request.aliases.length} type alias(es) from ${request.stub_dir}`);

    const results = projectComponents().definitionResolver.resolveFromStub(
      request.stub_dir,
      request.aliases,
    );

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
    case 'emit_surface':
      return handleEmitSurface(request);
    case 'capture_v2':
      return handleCaptureV2(request);
    case 'infer':
      return handleInfer(request);
    case 'build_workspace':
      return handleBuildWorkspace(request);
    case 'check_compatibility':
      return handleCheckCompatibility(request);
    case 'resolve_definitions':
      return handleResolveDefinitions(request);
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
 * Write a non-terminal progress/keepalive frame (async install protocol).
 * Distinct `status: 'progress'` so clients skip it and wait for the terminal
 * success/error frame.
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
