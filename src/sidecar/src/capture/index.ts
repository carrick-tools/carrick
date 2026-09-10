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
import * as os from 'node:os';
import type {
  AnchorOrigin,
  CaptureAliasRecord,
  CaptureFidelity,
  CaptureStubOptions,
  CaptureStubResult,
  SelfCheckOutcome,
  SerializationTier,
} from './api.js';
import { entryRelativeSpecifier, resolveAnchor, type ResolvedAnchor } from './anchors.js';
import { findAugmentationFiles } from './augmentations.js';
import { installedVersions, lockfileVersions } from './lockfile.js';
import { rewriteEmittedSpecifiers } from './paths-rewrite.js';
import { selfCheckStub } from './self-check.js';
import { collectSpecifiers, isRelative, packageNameOf } from './specifiers.js';
import { DenoProject, findDenoConfig } from './deno-project.js';

export type { CaptureStubOptions, CaptureStubResult } from './api.js';
export { DenoProject, findDenoConfig } from './deno-project.js';
// v2 check core ("tsc as the judge"). Same bundle, same seam: the sidecar
// reaches it only through this door (index.js).
export { runCheck } from './check.js';
export type { CheckProgress } from './check.js';

const SURFACE_ENTRY_BASENAME = '__carrick_surface__';

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
    ts_version: ts.version,
    errors,
  };
}

function emptyFidelity(): CaptureFidelity {
  return {
    total_aliases: 0,
    by_serialization: { emitted: 0, node_builder: 0, structural_fallback: 0 },
    by_self_check: { ok: 0, allowlisted_external: 0, decayed_internal: 0 },
    by_anchor_origin: { 'llm-symbol': 0, 'deterministic-infer': 0, 'anchor-backfill': 0 },
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

  const configPath = opts.tsconfigPath
    ? path.resolve(repoRoot, opts.tsconfigPath)
    : path.join(repoRoot, 'tsconfig.json');

  let parsed: ts.ParsedCommandLine | undefined;
  let deno: DenoProject | undefined;
  try {
    const config = findDenoConfig(repoRoot, opts.tsconfigPath ?? (fs.existsSync(configPath) ? configPath : undefined));
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
      ts.sys,
      repoRoot
    );
  } else {
    const configHost: ts.ParseConfigFileHost = {
      ...ts.sys,
      onUnRecoverableConfigFileDiagnostic: (d) => {
        throw new Error(ts.flattenDiagnosticMessageText(d.messageText, '\n'));
      },
    };
    try {
      parsed = ts.getParsedCommandLineOfConfigFile(configPath, {}, configHost) ?? undefined;
    } catch (err) {
      return fail(stubDir, packageName, [err instanceof Error ? err.message : String(err)]);
    }
  }
  if (!parsed) {
    return fail(stubDir, packageName, [`failed to parse ${configPath}`]);
  }

  // The surface entry must live inside the effective rootDir (design doc
  // Capture step 1: an entry at repo root with rootDir "src" fails TS6059).
  const entryDir = parsed.options.rootDir
    ? path.resolve(path.dirname(configPath), parsed.options.rootDir)
    : repoRoot;
  const entryPath = deno
    ? path.join(deno.cacheDir, `${SURFACE_ENTRY_BASENAME}.ts`)
    : path.join(entryDir, `${SURFACE_ENTRY_BASENAME}.ts`);
  fs.mkdirSync(path.dirname(entryPath), { recursive: true });

  // ---- Phase A: analysis program over placeholder entry + anchor sources ----
  let resolved: ResolvedAnchor[];
  try {
    resolved = resolveAnchors(opts, parsed, { repoRoot, entryDir: path.dirname(entryPath), entryPath }, deno);
  } catch (err) {
    return fail(stubDir, packageName, [err instanceof Error ? err.message : String(err)]);
  }

  // ---- Augmentation detection over the tsconfig's full file list ----
  const augmentationSources = [...new Set([...findAugmentationFiles(
    parsed.fileNames.filter((f) => !f.includes(`${path.sep}node_modules${path.sep}`))
  ), ...(deno?.globals ?? [])])];

  // ---- Phase B: declaration emit of the final entry ----
  const entryLines = ['// Generated by Carrick capture v2. Deleted after emit.'];
  for (const anchor of resolved) {
    const comment = anchor.failureReason
      ? ` // capture-degraded: ${anchor.failureReason.replace(/\n/g, ' ')}`
      : '';
    entryLines.push(
      `export type ${anchor.request.alias} = ${anchor.aliasText};${comment}`
    );
  }

  const staging = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-capture-v2-'));
  const emitted = new Map<string, string>();
  // Input .d.ts files (ambient stubs, augmentation declarations, local
  // hand-written declarations in the import closure) are never re-emitted by
  // tsc; they must ship verbatim or the tree's references to them dangle.
  const declarationSources = new Map<string, string>();
  const sourceByEmitted = new Map<string, string>();
  let emitPartial = false;
  try {
    fs.writeFileSync(entryPath, entryLines.join('\n') + '\n');
    const emitOptions: ts.CompilerOptions = {
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
    };
    const program = ts.createProgram([entryPath, ...augmentationSources], emitOptions, deno?.host(emitOptions));
    const emitResult = program.emit(
      undefined,
      (fileName, text, _bom, _error, sources) => {
        emitted.set(fileName, text);
        if (sources?.[0]) sourceByEmitted.set(path.relative(staging, fileName).split(path.sep).join('/'), sources[0].fileName);
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
      return fail(stubDir, packageName, ['declaration emit was skipped']);
    }
    emitPartial = emitResult.emitSkipped;
    for (const d of emitResult.diagnostics) {
      errors.push(ts.flattenDiagnosticMessageText(d.messageText, '\n'));
    }
    for (const sourceFile of program.getSourceFiles()) {
      if (!sourceFile.isDeclarationFile) continue;
      const abs = path.resolve(sourceFile.fileName);
      const rel = path.relative(entryDir, abs).split(path.sep).join('/');
      if (rel.startsWith('..') || rel.includes('node_modules/')) continue;
      declarationSources.set(rel, sourceFile.getFullText());
      sourceByEmitted.set(rel, abs);
    }
  } catch (err) {
    return fail(stubDir, packageName, [err instanceof Error ? err.message : String(err)]);
  } finally {
    if (fs.existsSync(entryPath)) fs.unlinkSync(entryPath);
    fs.rmSync(staging, { recursive: true, force: true });
  }

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
  if (emitPartial) {
    errors.push(
      `declaration emit was partial: kept ${emitted.size} emitted file(s); ` +
        'aliases referencing unemitted modules are demoted to structural_fallback'
    );
    resolved = demoteDanglingAliases({ resolved, emitted, declarationSources, staging });
  }

  // ---- Relocate the emitted tree into the stub package ----
  const typesDir = path.join(stubDir, 'types');
  fs.rmSync(stubDir, { recursive: true, force: true });
  fs.mkdirSync(typesDir, { recursive: true });

  const emittedFiles: string[] = [];
  let surfaceAbsPath = '';
  for (const [fileName, text] of emitted) {
    let rel = path.relative(staging, fileName).split(path.sep).join('/');
    if (path.basename(rel) === `${SURFACE_ENTRY_BASENAME}.d.ts`) {
      const source = sourceByEmitted.get(rel);
      sourceByEmitted.delete(rel);
      rel = 'surface.d.ts';
      if (source) sourceByEmitted.set(rel, source);
    }
    const dest = path.join(typesDir, rel);
    fs.mkdirSync(path.dirname(dest), { recursive: true });
    fs.writeFileSync(dest, text);
    emittedFiles.push(rel);
    if (rel === 'surface.d.ts') surfaceAbsPath = dest;
  }
  if (!surfaceAbsPath) {
    return fail(stubDir, packageName, ['no surface.d.ts produced by emit']);
  }
  // Verbatim copies of in-repo declaration sources (see declarationSources).
  for (const [rel, text] of declarationSources) {
    if (emittedFiles.includes(rel)) continue;
    const dest = path.join(typesDir, rel);
    fs.mkdirSync(path.dirname(dest), { recursive: true });
    fs.writeFileSync(dest, text);
    emittedFiles.push(rel);
  }

  // Tree-relative names of augmentation files that made it into the tree
  // (.ts augmentations arrive via declaration emit, .d.ts ones verbatim).
  const augmentationFiles = augmentationSources
    .map((abs) => {
      const noDts = abs.replace(/\.d\.ts$/, '');
      const noExt = noDts === abs ? abs.replace(/\.(ts|tsx|mts|cts)$/, '') : noDts;
      const rel = path.relative(entryDir, noExt).split(path.sep).join('/');
      return `${rel}.d.ts`;
    })
    .filter((rel) => emittedFiles.includes(rel))
    .map((rel) => `types/${rel}`);

  // ---- Post-emit specifier rewrite (paths mappings + absolute internals) ----
  let denoRewrites = 0;
  try {
    denoRewrites = deno?.rewrite(typesDir, emittedFiles, sourceByEmitted) ?? 0;
  } catch (err) {
    return fail(stubDir, packageName, [err instanceof Error ? err.message : String(err)]);
  }
  const specifierRewrites = denoRewrites + rewriteEmittedSpecifiers({
    typesDir,
    files: emittedFiles,
    options: parsed.options,
    configPath,
    entryDir,
  });

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
  // the directly-referenced externals; transitives resolve at check-install
  // (check-workspace NPMRC: "Direct deps are exact-pinned by the stubs;
  // only transitives resolve").
  const installed = installedVersions(repoRoot, externalSpecs);
  const lockVersions = lockfileVersions(repoRoot);
  const pinned: Record<string, string> = {};
  const unpinned: string[] = [];
  for (const name of [...externalSpecs].sort()) {
    const version = deno?.pinned[name] ?? installed.get(name) ?? lockVersions.get(name);
    if (version) pinned[name] = version;
    else unpinned.push(name);
  }

  const dependencyRoot = deno?.config.workspaceRoot ?? repoRoot;
  const bareCheckout = !fs.existsSync(path.join(dependencyRoot, 'node_modules'));

  fs.writeFileSync(
    path.join(stubDir, 'package.json'),
    JSON.stringify(
      {
        name: packageName,
        version: '0.0.0-carrick',
        private: true,
        types: './types/surface.d.ts',
        dependencies: pinned,
      },
      null,
      2
    ) + '\n'
  );
  fs.writeFileSync(
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

  // ---- Capture-time self-check (per-alias closure attribution) ----
  const aliases = selfCheckStub({
    stubDir,
    surfaceAbsPath,
    resolved,
    pinned,
    bareCheckout,
    repoRoot: dependencyRoot,
  });
  const fidelity = computeFidelity(aliases);

  fs.writeFileSync(
    path.join(stubDir, 'carrick-manifest.json'),
    JSON.stringify(
      {
        package_name: packageName,
        ts_version: ts.version,
        bare_checkout: bareCheckout,
        aliases,
        fidelity,
      },
      null,
      2
    ) + '\n'
  );

  return {
    success: true,
    stub_dir: stubDir,
    package_name: packageName,
    emitted_files: emittedFiles.map((rel) => `types/${rel}`).sort(),
    pinned_dependencies: pinned,
    unpinned_externals: unpinned,
    aliases,
    fidelity,
    augmentation_files: augmentationFiles.sort(),
    specifier_rewrites: specifierRewrites,
    bare_checkout: bareCheckout,
    ts_version: ts.version,
    errors,
  };
}

/**
 * Partial-emit demotion: with the set of modules that DID reach the tree
 * (emitted .d.ts plus verbatim declaration sources), demote every anchor
 * whose alias text references a relative module absent from that set, and
 * rewrite the demoted aliases' lines in the emitted surface to `unknown`.
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
}): ResolvedAnchor[] {
  // Extensionless, entryDir-relative POSIX module ids present in the tree.
  const treeModules = new Set<string>();
  let surfaceKey: string | undefined;
  for (const fileName of args.emitted.keys()) {
    const rel = path.relative(args.staging, fileName).split(path.sep).join('/');
    if (path.basename(rel) === `${SURFACE_ENTRY_BASENAME}.d.ts`) surfaceKey = fileName;
    if (rel.endsWith('.d.ts')) treeModules.add(rel.slice(0, -'.d.ts'.length));
  }
  for (const rel of args.declarationSources.keys()) {
    if (rel.endsWith('.d.ts')) treeModules.add(rel.slice(0, -'.d.ts'.length));
  }
  // Deno's temporary surface lives in the cache inside the emitted tree.
  const surfaceDir = surfaceKey ? path.posix.dirname(path.relative(args.staging, surfaceKey).split(path.sep).join('/')) : '.';
  const moduleInTree = (spec: string): boolean => {
    const id = path.posix.normalize(path.posix.join(surfaceDir, spec));
    if (id.startsWith('..')) return false;
    return treeModules.has(id) || treeModules.has(`${id}/index`);
  };

  const demoted = new Set<string>();
  const next = args.resolved.map((anchor): ResolvedAnchor => {
    if (anchor.failureReason !== undefined) return anchor;
    const dangling = [...collectSpecifiers(anchor.aliasText)].find(
      (spec) => isRelative(spec) && !moduleInTree(spec)
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

/** Phase A: build the placeholder entry, then resolve every anchor. */
function resolveAnchors(
  opts: CaptureStubOptions,
  parsed: ts.ParsedCommandLine,
  ctx: { repoRoot: string; entryDir: string; entryPath: string },
  deno?: DenoProject,
): ResolvedAnchor[] {
  const placeholderLines = ['// Carrick capture v2 analysis placeholder.'];
  for (const anchor of opts.anchors) {
    placeholderLines.push(`export type ${anchor.alias} = unknown;`);
  }

  fs.writeFileSync(ctx.entryPath, placeholderLines.join('\n') + '\n');
  try {
    const anchorSources = [
      ...new Set(
        opts.anchors
          .filter((a) => a.kind !== 'literal')
          .map((a) => path.join(ctx.repoRoot, a.source_file))
      ),
    ].filter((f) => fs.existsSync(f));
    const options = {
      ...parsed.options,
      noEmit: true,
    };
    const program = ts.createProgram([ctx.entryPath, ...anchorSources, ...(deno?.globals ?? [])], options, deno?.host(options));
    const entrySource = program.getSourceFile(ctx.entryPath);
    const placeholders = new Map<string, ts.TypeAliasDeclaration>();
    if (entrySource) {
      for (const stmt of entrySource.statements) {
        if (ts.isTypeAliasDeclaration(stmt)) placeholders.set(stmt.name.text, stmt);
      }
    }
    // A literal anchor whose text is a bare identifier resolves through a
    // sibling symbol anchor's module when one names the same symbol.
    const siblingSymbolSpecs = new Map<string, string>();
    for (const anchor of opts.anchors) {
      if (anchor.kind !== 'symbol') continue;
      if (!siblingSymbolSpecs.has(anchor.symbol_name)) {
        siblingSymbolSpecs.set(
          anchor.symbol_name,
          entryRelativeSpecifier(ctx.entryDir, ctx.repoRoot, anchor.source_file)
        );
      }
    }
    return opts.anchors.map((request) =>
      resolveAnchor(program, request, {
        repoRoot: ctx.repoRoot,
        entryDir: ctx.entryDir,
        placeholder: placeholders.get(request.alias),
        siblingSymbolSpecs,
      })
    );
  } finally {
    if (fs.existsSync(ctx.entryPath)) fs.unlinkSync(ctx.entryPath);
  }
}
