/**
 * Capture-time self-check (design doc, Capture step 8, amendment 1), with
 * per-alias closure attribution.
 *
 * The stub tree is typechecked standalone with `skipLibCheck: false` --
 * spike-verified as load-bearing: the tree is entirely .d.ts and skipLibCheck
 * skips declaration files wholesale, making the gate vacuous. It also sets
 * `skipDefaultLibCheck: true`, which skips only the compiler's own default
 * lib files, never a stub file (see `SELF_CHECK_OPTIONS`). When the
 * source repo has node_modules, resolution is pointed at it via a temporary
 * node_modules symlink inside the stub, so externals resolve exactly as they
 * will at check time against installed pins.
 *
 * Classification per alias (three-way, keyed on diagnostics, never on
 * printed type text):
 *  - ok: resolves to a concrete type.
 *  - allowlisted_external: resolution failed only through external
 *    specifiers pinned in the stub's dependencies, on a bare checkout. The
 *    alias KEEPS its serialization tier; the check-phase probe gates
 *    (any/unknown/never, both sides) are the backstop.
 *  - decayed_internal: a dangling internal specifier, an unpinned external,
 *    or a top-type resolution with no allowlisted explanation.
 *
 * Attribution is per-alias closure: failed specifiers are blamed on an alias
 * only if they occur in a file reachable from that alias's surface statement
 * (import-type seeds, then BFS over relative imports). The spike's
 * file-granularity shortcut is gone -- including on the SURFACE file itself,
 * which holds every alias, so a file-granular bucket there blamed the whole
 * service for one alias's dangling specifier (cloud#1184). A surface
 * diagnostic is attributed by `export type` statement span, exactly as
 * check-poison.ts contains poison; one that no statement covers keeps the
 * service-wide bucket.
 */

import ts from 'typescript';
import { createHash } from 'node:crypto';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { findAugmentationFiles } from './augmentations.js';
import { sidecarCompilerOptions } from './compiler-options.js';
import type { CaptureAliasRecord, CaptureAnchorRequest, SelfCheckOutcome } from './api.js';
import type { ResolvedAnchor } from './anchors.js';
import {
  collectSpecifiers,
  declarationCandidates,
  isDeclarationFileName,
  isRelative,
  packageNameOf,
} from './specifiers.js';
import { repairDanglingImports, type RepairedFile } from './repair-dangling.js';
import { RESOLUTION_FILE } from './resolution-edges.js';
import type { WriteGuard } from './guarded-fs.js';
import {
  findDisqualifyingTopTypes,
  findUnresolvedPlaceholders,
  isErrorPlaceholder,
  provenanceOf,
  unemittedModuleProvenance,
  unresolvedInTreeProvenance,
  type DeepTopType,
} from './deep-walk.js';

/** One surface entry of the tree and the anchors its aliases came from. */
export interface SelfCheckSurface {
  surfaceAbsPath: string;
  resolved: ResolvedAnchor[];
}

export interface SelfCheckArgs {
  /** Writes are held to the stub (carrick#1748). */
  guard: WriteGuard;
  stubDir: string;
  /**
   * The surfaces checked over the tree, each with records of its own. Two when
   * a capture's literal backfill holds the same tree as its first surface
   * (carrick#2041): the second sits beside the first, a module nothing
   * imports, so neither changes what the other's aliases read.
   */
  surfaces: SelfCheckSurface[];
  pinned: Record<string, string>;
  bareCheckout: boolean;
  /** Producer repo root; its node_modules (if any) backs resolution. */
  repoRoot: string;
  compilerHost?: (options: ts.CompilerOptions) => ts.CompilerHost;
}

interface FileFailures {
  externalPinned: Set<string>;
  internal: Set<string>;
  /** Identifiers the file (or, on the surface, the alias statement) names
   * that the compiler cannot find: the names a placeholder stands for
   * (carrick#1446). */
  unfoundNames: Set<string>;
}

/** The records of each surface, in `args.surfaces` order. */
export function selfCheckStub(args: SelfCheckArgs): CaptureAliasRecord[][] {
  const typesDir = path.join(args.stubDir, 'types');
  const treeFiles: string[] = [];
  const walk = (dir: string) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const p = path.join(dir, entry.name);
      if (entry.isDirectory()) walk(p);
      else if (isDeclarationFileName(entry.name)) treeFiles.push(p);
    }
  };
  walk(typesDir);

  // Point resolution at the producer repo's installed deps when present.
  const repoNodeModules = path.join(args.repoRoot, 'node_modules');
  const linkPath = path.join(args.stubDir, 'node_modules');
  let linked = false;
  if (!args.bareCheckout && fs.existsSync(repoNodeModules) && !fs.existsSync(linkPath)) {
    args.guard.symlink(repoNodeModules, linkPath, 'dir');
    linked = true;
  }

  try {
    const first = runSelfCheck(args, treeFiles);
    // carrick#1397: an emitted declaration that imports a module the checkout
    // does not have makes every alias reaching that file unpublishable, however
    // much of it resolved. Repair the file — drop the import, write `unknown`
    // where its names were used — and check the tree again. The second pass is
    // the verdict: it reads the repaired text, and a name the rewrite did not
    // reach comes back as a `Cannot find name` diagnostic that
    // `repairedNameFailures` folds back into the same dangling specifier.
    const repaired = repairDanglingImports(first.internalFailuresByFile, args.guard.narrow(typesDir));
    if (repaired.size === 0) return first.records;
    return runSelfCheck(args, treeFiles, repaired).records;
  } finally {
    // unlinkSync, not rmSync: the link target is a directory and rmSync
    // refuses symlinks-to-directories with EISDIR.
    if (linked) args.guard.unlink(linkPath);
  }
}

/** One self-check pass: its records, and what each file could not resolve. */
interface SelfCheckPass {
  /** Per surface, in `args.surfaces` order. */
  records: CaptureAliasRecord[][];
  /**
   * Absolute file -> internal specifiers that failed in it, for the repair
   * pass. The surface files are excluded: their failures are the pasted text
   * of a literal anchor (carrick#1361), not a declaration to repair.
   */
  internalFailuresByFile: Map<string, Set<string>>;
}

/**
 * The options of every self-check program.
 *
 * `skipLibCheck: false` keeps every stub `.d.ts` checked (see module header).
 * `skipDefaultLibCheck: true` skips only files the program loaded as a default
 * lib (the default lib, `lib` entries, `/// <reference lib>` targets), which
 * was most of a capture's check time (carrick#2158). It is safe here because
 * the self-check reads only resolution diagnostics (2307, 2792, `Cannot find
 * name|namespace`), attributes them only through the stub tree, and the
 * compiler reports each at the referencing stub node when it checks that stub.
 *
 * It must NOT be set on the judge's `tsc` (`CHECKER_TSCONFIG`): some checks of
 * a merged interface run only when its FIRST declaration is checked, so a stub
 * that augments `interface Window { 0: number }` loses its TS2411 when
 * `lib.dom.d.ts` is skipped, and the judge counts any stub diagnostic.
 */
export const SELF_CHECK_OPTIONS: Readonly<ts.CompilerOptions> = sidecarCompilerOptions({
  noEmit: true,
  strict: true,
  // MUST be false: the whole stub tree is .d.ts (see module header).
  skipLibCheck: false,
  skipDefaultLibCheck: true,
  module: ts.ModuleKind.ESNext,
  moduleResolution: ts.ModuleResolutionKind.Bundler,
  types: [],
});

function runSelfCheck(
  args: SelfCheckArgs,
  treeFiles: string[],
  repaired?: Map<string, RepairedFile>
): SelfCheckPass {
  const program = ts.createProgram(treeFiles, SELF_CHECK_OPTIONS, args.compilerHost?.(SELF_CHECK_OPTIONS));
  const checker = program.getTypeChecker();
  const diagnostics = ts.getPreEmitDiagnostics(program);

  // A surface holds EVERY alias, and every alias's closure starts there, so
  // a file-granular failure bucket on it blames the whole service for one
  // alias's dangling specifier (cloud#1184). Attribute by `export type`
  // statement span, exactly as check-poison.ts contains poison.
  const surfaces = args.surfaces.map((surface) => {
    const surfaceAbs = path.resolve(surface.surfaceAbsPath);
    const surfaceSource = program.getSourceFile(surfaceAbs);
    return {
      surfaceAbs,
      surfaceSource,
      resolved: surface.resolved,
      aliasAtSurfacePosition: buildSurfaceSpanIndex(surfaceSource),
      // Failed module specifiers on the surface, per ALIAS.
      surfaceFailuresByAlias: new Map<string, FileFailures>(),
    };
  });
  const surfaceAt = new Map(surfaces.map((surface) => [surface.surfaceAbs, surface]));

  // Failed module specifiers, split external-pinned vs internal, per FILE —
  // except on a surface, where they are per ALIAS.
  const failuresByFile = new Map<string, FileFailures>();
  const emptyFailures = (): FileFailures => ({
    externalPinned: new Set(),
    internal: new Set(),
    unfoundNames: new Set(),
  });
  const bucketIn = (map: Map<string, FileFailures>, key: string): FileFailures => {
    let entry = map.get(key);
    if (!entry) {
      entry = emptyFailures();
      map.set(key, entry);
    }
    return entry;
  };
  // A surface diagnostic outside every alias statement (a file-level import,
  // a reference directive) is attributable to no alias and keeps the
  // service-wide file bucket: soundness over precision, the same fallback
  // check-poison.ts makes.
  const bucketFor = (abs: string, start: number | undefined): FileFailures => {
    const surface = surfaceAt.get(abs);
    const owner = surface?.aliasAtSurfacePosition(start);
    return surface && owner ? bucketIn(surface.surfaceFailuresByAlias, owner) : bucketIn(failuresByFile, abs);
  };
  for (const d of diagnostics) {
    if (!d.file) continue;
    const abs = path.resolve(d.file.fileName);
    const msg = ts.flattenDiagnosticMessageText(d.messageText, ' ');
    // A name the compiler cannot find is the placeholder's cause wherever a
    // type uses it; the record names it beside those positions (carrick#1446).
    const unfound = /Cannot find (?:name|namespace) '([^']+)'/.exec(msg);
    if (unfound) bucketFor(abs, d.start).unfoundNames.add(unfound[1]);
    // A name the repair did not reach: the import that bound it is gone, so
    // the module is no longer reported missing and only this diagnostic is
    // left to say the tree is incomplete. Blamed on the specifier that bound
    // the name, which is the sentence a reader can act on (carrick#1397).
    const orphaned = repairedNameFailure(repaired, abs, d, msg);
    if (orphaned) {
      bucketIn(failuresByFile, abs).internal.add(orphaned);
      continue;
    }
    if (d.code !== 2307 && d.code !== 2792) continue;
    const m = /Cannot find module '([^']+)'/.exec(msg);
    if (!m) continue;
    const spec = m[1];
    const bucket = bucketFor(abs, d.start);
    if (!isRelative(spec) && args.pinned[packageNameOf(spec)]) {
      bucket.externalPinned.add(spec);
    } else {
      bucket.internal.add(spec);
    }
  }

  // Per-file relative-import adjacency for closure walks.
  const adjacency = new Map<string, string[]>();
  const textOf = new Map<string, string>();
  const hashAdjacency = new Map<string, string[]>();
  const treeSet = new Set(treeFiles.map((f) => path.resolve(f)));
  for (const file of treeFiles) {
    const abs = path.resolve(file);
    const text = fs.readFileSync(abs, 'utf8');
    textOf.set(abs, text);
    const neighbors: string[] = [];
    for (const spec of collectSpecifiers(text)) {
      const resolved = resolveTreeSpecifier(abs, spec, treeSet);
      if (resolved) neighbors.push(resolved);
    }
    adjacency.set(abs, neighbors);
    // The hash's closure (carrick#3135) also follows what `collectSpecifiers`
    // does not read: `import x = require("./y")` and reference paths. It
    // must hold every file the judge loads for the alias.
    const more: string[] = [];
    for (const re of [/require\s*\(\s*["']([^"']+)["']/g, /<reference\s+path\s*=\s*["']([^"']+)["']/g]) {
      for (const m of text.matchAll(re)) {
        const resolved =
          resolveTreeSpecifier(abs, m[1], treeSet) ??
          (treeSet.has(path.resolve(path.dirname(abs), m[1])) ? path.resolve(path.dirname(abs), m[1]) : undefined);
        if (resolved) more.push(resolved);
      }
    }
    hashAdjacency.set(abs, [...neighbors, ...more]);
  }
  const judgeInputs = stubJudgeInputs(args, program, textOf, [...surfaceAt.keys()]);

  // Demotions (failureReason present) never reached the surface with a real
  // type; everything else — including successful literal anchors, which sit
  // at the structural_fallback TIER but did produce text — is classified by
  // the real self-check.
  const records = surfaces.map((surface) =>
    surface.resolved.map((anchor) =>
      anchor.failureReason !== undefined
        ? demotedRecord(anchor)
        : checkedRecord(anchor, {
            args,
            program,
            checker,
            surfaceAbs: surface.surfaceAbs,
            surfaceSource: surface.surfaceSource,
            adjacency,
            failuresByFile,
            surfaceFailuresByAlias: surface.surfaceFailuresByAlias,
            textOf,
            hashAdjacency,
            judgeInputs,
          })
    )
  );

  const internalFailuresByFile = new Map<string, Set<string>>();
  for (const [file, failures] of failuresByFile) {
    // A surface's own failures are a literal anchor's pasted text
    // (carrick#1361), not a declaration with an import to drop.
    if (surfaceAt.has(file) || failures.internal.size === 0) continue;
    // A file already repaired has had its turn: repairing it again would chase
    // its own leftover diagnostics.
    if (repaired?.has(file)) continue;
    internalFailuresByFile.set(file, failures.internal);
  }
  return { records, internalFailuresByFile };
}

/**
 * The specifier to blame for a `Cannot find name` diagnostic in a file this
 * capture repaired, when the name is one the dropped import bound.
 *
 * The rewrite writes `unknown` at every type position it can reach; a use it
 * cannot — and there should be none, since a position where `unknown` is not a
 * type leaves the whole file untouched — would otherwise read as a clean tree,
 * because the module that is missing is no longer imported to be reported.
 * Fail closed: the alias keeps the refusal it had before the repair.
 */
/**
 * Detail for a pinned external that does not resolve on an installed checkout.
 * A subpath on the specifier means the rewrite found no entry of the package
 * exporting a declaration from that file, so it named the file itself.
 */
function unresolvedExternalDetail(spec: string): string {
  const pkg = packageNameOf(spec);
  const subpath = spec.slice(pkg.length + 1);
  const base = `unresolved pinned external '${spec}' on an installed checkout`;
  return subpath ? `${base}: no entry of '${pkg}' exports a declaration from '${subpath}'` : base;
}

function repairedNameFailure(
  repaired: Map<string, RepairedFile> | undefined,
  file: string,
  diagnostic: ts.Diagnostic,
  message: string
): string | undefined {
  if (!repaired || diagnostic.code !== 2304) return undefined;
  const entry = repaired.get(file);
  if (!entry) return undefined;
  const named = /Cannot find name '([^']+)'/.exec(message);
  if (!named || !entry.names.includes(named[1])) return undefined;
  return entry.specifiers[0];
}

/**
 * Position -> the alias whose `export type` statement span covers it, for
 * diagnostics reported on the surface file. `undefined` when no alias
 * statement covers the position, or when the surface is not in the program.
 */
function buildSurfaceSpanIndex(
  surfaceSource: ts.SourceFile | undefined
): (position: number | undefined) => string | undefined {
  if (!surfaceSource) return () => undefined;
  const spans: Array<{ alias: string; start: number; end: number }> = [];
  for (const stmt of surfaceSource.statements) {
    if (!ts.isTypeAliasDeclaration(stmt)) continue;
    spans.push({
      alias: stmt.name.text,
      start: stmt.getStart(surfaceSource),
      end: stmt.getEnd(),
    });
  }
  return (position) => {
    if (position === undefined) return undefined;
    return spans.find((span) => position >= span.start && position <= span.end)?.alias;
  };
}

/**
 * A literal anchor passes the inferrer's marks to its record, where the check
 * phase reads them: a body read as raw text (carrick#1842), and a body whose
 * handler works when none is sent (carrick#2814). A symbol anchor passes the
 * second only: the model named the body's type, the handler is still read.
 */
function literalMarks(
  request: CaptureAnchorRequest
): Pick<CaptureAliasRecord, 'raw_text_read' | 'absent_body_tolerated'> {
  if (request.kind === 'symbol') {
    return request.absent_body_tolerated ? { absent_body_tolerated: true } : {};
  }
  if (request.kind !== 'literal') return {};
  return {
    ...(request.raw_text_read ? { raw_text_read: true } : {}),
    ...(request.absent_body_tolerated ? { absent_body_tolerated: true } : {}),
  };
}

/** An alias that never reached a capture-native tier: the failure reason was
 * recorded at demotion time; the surface line is `unknown` by construction. */
function demotedRecord(anchor: ResolvedAnchor): CaptureAliasRecord {
  return {
    alias: anchor.request.alias,
    anchor_kind: anchor.request.kind,
    symbol_name: 'symbol_name' in anchor.request ? anchor.request.symbol_name : undefined,
    source_file: anchor.request.kind === 'literal' ? '<inline>' : anchor.request.source_file,
    anchor_origin: anchor.request.anchor_origin,
    serialization: 'structural_fallback',
    self_check: 'decayed_internal',
    self_check_detail: anchor.failureReason,
    capture_failure_reason: anchor.failureReason,
    top_type_at_self_check: true,
    // A literal's text is what the index serves for it, and a module it
    // names never reached the tree (carrick#1446). A symbol anchor's served
    // declaration comes from elsewhere, so its demotion says nothing about it.
    ...(anchor.request.kind === 'literal' && anchor.namesUnemittedModule
      ? { unresolved_in_tree: [unemittedModuleProvenance()] }
      : {}),
    ...literalMarks(anchor.request),
  };
}

function checkedRecord(
  anchor: ResolvedAnchor,
  ctx: {
    args: SelfCheckArgs;
    program: ts.Program;
    checker: ts.TypeChecker;
    surfaceAbs: string;
    surfaceSource: ts.SourceFile | undefined;
    adjacency: Map<string, string[]>;
    failuresByFile: Map<string, FileFailures>;
    surfaceFailuresByAlias: Map<string, FileFailures>;
    textOf: Map<string, string>;
    hashAdjacency: Map<string, string[]>;
    judgeInputs: StubJudgeInputs;
  }
): CaptureAliasRecord {
  const alias = anchor.request.alias;
  let topType = true;
  let deepFindings: DeepTopType[] = [];
  // Where the alias's type holds the unresolved-reference placeholder in this
  // tree (carrick#1446): `''` for its own type. The deep walk leaves the
  // placeholder out because the check heals a pinned external; what the index
  // publishes is this tree, so the record says where it does not resolve.
  let unresolvedPaths: string[] = [];
  const seeds: string[] = [];
  let statementText: string | undefined;

  if (ctx.surfaceSource) {
    for (const stmt of ctx.surfaceSource.statements) {
      if (!ts.isTypeAliasDeclaration(stmt) || stmt.name.text !== alias) continue;
      statementText = stmt.getText(ctx.surfaceSource);
      const type = ctx.checker.getTypeAtLocation(stmt.name);
      topType =
        (type.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown | ts.TypeFlags.Never)) !== 0;
      if (!topType) {
        deepFindings = findDisqualifyingTopTypes(
          type,
          ctx.program,
          ctx.checker,
          stmt.name
        );
        unresolvedPaths = findUnresolvedPlaceholders(type, ctx.program, ctx.checker, stmt.name);
      } else if (isErrorPlaceholder(type)) {
        unresolvedPaths = [''];
      }
      // Seed the closure with the alias's own import-type targets.
      const visit = (node: ts.Node) => {
        if (ts.isImportTypeNode(node) && ts.isLiteralTypeNode(node.argument)) {
          const lit = node.argument.literal;
          if (ts.isStringLiteral(lit) && isRelative(lit.text)) {
            const resolved = resolveTreeSpecifier(
              ctx.surfaceAbs,
              lit.text,
              new Set(ctx.adjacency.keys())
            );
            if (resolved) seeds.push(resolved);
          }
        }
        node.forEachChild(visit);
      };
      visit(stmt);
    }
  }

  // Closure = surface itself + BFS from the alias's import-type seeds.
  const closure = new Set<string>([ctx.surfaceAbs]);
  const queue = [...seeds];
  while (queue.length > 0) {
    const file = queue.pop()!;
    if (closure.has(file)) continue;
    closure.add(file);
    for (const next of ctx.adjacency.get(file) ?? []) queue.push(next);
  }

  let blamedExternal: string | undefined;
  let internalFailure: string | undefined;
  const danglingSpecifiers = new Set<string>();
  const unfoundNames = new Set<string>();
  // This alias's own surface statement, then the closure's files. The surface
  // file bucket now holds only the diagnostics no alias statement covers.
  const closureFailures = [
    ctx.surfaceFailuresByAlias.get(alias),
    ...[...closure].map((file) => ctx.failuresByFile.get(file)),
  ];
  for (const failures of closureFailures) {
    if (!failures) continue;
    if (!blamedExternal) blamedExternal = [...failures.externalPinned][0];
    if (!internalFailure) internalFailure = [...failures.internal][0];
    for (const specifier of failures.internal) danglingSpecifiers.add(specifier);
    for (const name of failures.unfoundNames) unfoundNames.add(name);
  }

  // Classification consults the closure failures REGARDLESS of the root
  // type: a dangling internal specifier means part of this alias's closure
  // is missing and the emitted tree is silently wrong even when the root
  // resolves to a concrete shape (adversarial-review finding 1).
  let outcome: SelfCheckOutcome = 'ok';
  let detail: string | undefined;
  const allowlisted = (): void => {
    const pkg = packageNameOf(blamedExternal!);
    outcome = 'allowlisted_external';
    detail =
      `unresolved external '${blamedExternal}' is pinned ` +
      `(${pkg}@${ctx.args.pinned[pkg]}); kept at tier ${anchor.serialization} ` +
      'on bare checkout; probe gates are the backstop';
  };
  // `unexplainedDeep`: a member-level author-baked any/unknown. The deep walk
  // excludes TypeScript's unresolved-reference `error` placeholder (see
  // `flagOf`), so `deep` is ALWAYS an author-baked disqualifier that survives
  // into the emitted `.d.ts` text — never a healable external decay. The check
  // phase must pre-gate exactly these: its probe gates are whole-type only and
  // cannot see a member-level any.
  let unexplainedDeep: DeepTopType[] = [];
  const deep = deepFindings[0];
  if (internalFailure) {
    outcome = 'decayed_internal';
    detail = `dangling internal specifier '${internalFailure}'`;
  } else if (topType || deep) {
    // `deepFindings` is only computed when `!topType`, so the two are mutually
    // exclusive here.
    if (deep) {
      // A member-level disqualifier. Either an author-baked any/unknown (baked
      // into the emitted text; does NOT heal when the check installs pins) or a
      // `budget_exhausted` sentinel (a subtree too deep/wide to finish). Record
      // it even when the closure ALSO carries a failing pinned external (which
      // would otherwise allowlist the alias) — fail closed.
      unexplainedDeep = deepFindings;
      outcome = 'decayed_internal';
      detail =
        deep.kind === 'budget_exhausted'
          ? `type too deep or wide to verify within the capture budget at ` +
            `'${deep.path}'; abstaining (fail closed) rather than risk a buried ` +
            'any reading compatible'
          : `type carries '${deep.kind}' at '${deep.path}'; an arbitrary ` +
            'counterparty shape would read compatible';
    } else if (blamedExternal) {
      // Root top type from a pinned external decay: heals when the check
      // installs the pin.
      if (ctx.args.bareCheckout) {
        allowlisted();
      } else {
        outcome = 'decayed_internal';
        detail = unresolvedExternalDetail(blamedExternal);
      }
    } else {
      // Root top type with no external explanation.
      outcome = 'decayed_internal';
      detail = 'alias resolved to a top type with no allowlisted external failure';
    }
  } else if (blamedExternal) {
    // The alias's own type is fully concrete, but its closure has failing
    // pinned-external specifiers — previously invisible behind the topType
    // gate. On a bare checkout that is the expected amendment-1 shape; on an
    // installed checkout it means the tree references something the repo's
    // own node_modules could not resolve.
    if (ctx.args.bareCheckout) {
      allowlisted();
    } else {
      outcome = 'decayed_internal';
      detail = unresolvedExternalDetail(blamedExternal);
    }
  }

  const record: CaptureAliasRecord = {
    alias,
    anchor_kind: anchor.request.kind,
    symbol_name: 'symbol_name' in anchor.request ? anchor.request.symbol_name : undefined,
    source_file: anchor.request.kind === 'literal' ? '<inline>' : anchor.request.source_file,
    anchor_origin: anchor.request.anchor_origin,
    serialization: anchor.serialization,
    self_check: outcome,
    // A #438/#439 re-aim note is provenance only; a real decay detail always
    // wins, so it surfaces exactly when the re-aimed alias self-checks clean.
    // An abstain (carrick#766) is the most specific statement available: the
    // surface line is `unknown` because this layer wrote it that way, so its
    // reason outranks whatever the closure walk blames.
    self_check_detail: anchor.abstainReason ?? detail ?? anchor.reaimNote,
    top_type_at_self_check: topType,
    ...(unexplainedDeep.length > 0
      ? {
          any_provenance: unexplainedDeep.map((finding) =>
            provenanceOf(finding, anchor.unresolved)
          ),
        }
      : {}),
    ...(danglingSpecifiers.size > 0
      ? { dangling_specifiers: [...danglingSpecifiers].sort() }
      : {}),
    ...(anchor.undeclaredNames ? { undeclared_names: anchor.undeclaredNames } : {}),
    ...(unresolvedPaths.length > 0
      ? {
          unresolved_in_tree: [...unresolvedPaths]
            .sort()
            .map((p) => unresolvedInTreeProvenance(p, [...unfoundNames].sort())),
        }
      : {}),
    ...literalMarks(anchor.request),
  };
  return statementText === undefined
    ? record
    : {
        ...record,
        surface_hash: surfaceHash(record, statementText, closure, ctx),
      };
}

/**
 * What every alias of one stub hashes beside its own closure (carrick#3135):
 * the surface's statements that declare no alias, the tree files in global
 * scope, and the stub's pinned externals and recorded dependency edges.
 */
interface StubJudgeInputs {
  /** Per surface: the text of its statements that are not type aliases. */
  surfaceRest: Map<string, string[]>;
  /** Stub-relative path and text of each global-scope tree file, sorted. */
  globals: Array<[string, string]>;
  pinned: Array<[string, string]>;
  resolution: string | null;
  bareCheckout: boolean;
}

/**
 * A tree file in global scope reaches every alias the judge loads beside
 * it, closure or not: a script (no import or export), a file that declares
 * a global or module augmentation, and a file with a reference directive.
 */
function stubJudgeInputs(
  args: SelfCheckArgs,
  program: ts.Program,
  textOf: Map<string, string>,
  surfaces: string[]
): StubJudgeInputs {
  const augmenting = new Set(findAugmentationFiles([...textOf.keys()]).map((f) => path.resolve(f)));
  const globals: Array<[string, string]> = [];
  for (const [abs, text] of textOf) {
    if (surfaces.includes(abs)) continue;
    const source = program.getSourceFile(abs);
    const global =
      augmenting.has(abs) ||
      /^\s*\/\/\/\s*<reference\b/m.test(text) ||
      (source !== undefined && !ts.isExternalModule(source));
    if (global) globals.push([stubRelative(args.stubDir, abs), text]);
  }
  globals.sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  const surfaceRest = new Map<string, string[]>();
  for (const surface of surfaces) {
    const source = program.getSourceFile(surface);
    surfaceRest.set(
      surface,
      [
        ...(source?.statements ?? [])
          .filter((stmt) => !ts.isTypeAliasDeclaration(stmt))
          .map((stmt) => stmt.getText(source)),
        ...(source?.referencedFiles ?? []).map((ref) => `path:${ref.fileName}`),
        ...(source?.typeReferenceDirectives ?? []).map((ref) => `types:${ref.fileName}`),
        ...(source?.libReferenceDirectives ?? []).map((ref) => `lib:${ref.fileName}`),
      ]
    );
  }
  let resolution: string | null = null;
  try {
    resolution = fs.readFileSync(path.join(args.stubDir, RESOLUTION_FILE), 'utf8');
  } catch {
    // No recorded edges: the check resolves every transitive from the registry.
  }
  return {
    surfaceRest,
    globals,
    pinned: Object.entries(args.pinned).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)),
    resolution,
    bareCheckout: args.bareCheckout,
  };
}

function stubRelative(stubDir: string, abs: string): string {
  return path.relative(stubDir, abs).split(path.sep).join('/');
}

/**
 * carrick#3135: the hash of everything the judge reads for one alias from its
 * stub. No machine path enters it, so two scans of one tree at different
 * checkout paths hash alike. File-granular: the closure's files are hashed
 * whole, so an unrelated declaration in a reached file moves the hash (fails
 * toward judging). A closure file that imports the surface itself reaches
 * every alias, so the whole surface is hashed then.
 */
function surfaceHash(
  record: CaptureAliasRecord,
  statementText: string,
  closure: Set<string>,
  ctx: {
    args: SelfCheckArgs;
    surfaceAbs: string;
    textOf: Map<string, string>;
    hashAdjacency: Map<string, string[]>;
    judgeInputs: StubJudgeInputs;
  }
): string {
  const reached = new Set(closure);
  const queue = [...closure].filter((file) => file !== ctx.surfaceAbs);
  while (queue.length > 0) {
    for (const next of ctx.hashAdjacency.get(queue.pop()!) ?? []) {
      if (reached.has(next)) continue;
      reached.add(next);
      queue.push(next);
    }
  }
  const files: Array<[string, string]> = [];
  let surfaceReached = false;
  for (const file of reached) {
    if (file === ctx.surfaceAbs) continue;
    if ((ctx.hashAdjacency.get(file) ?? []).includes(ctx.surfaceAbs)) surfaceReached = true;
    files.push([stubRelative(ctx.args.stubDir, file), ctx.textOf.get(file) ?? '']);
  }
  files.sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  const inputs = ctx.judgeInputs;
  const preimage = {
    statement: statementText,
    surface_rest: inputs.surfaceRest.get(ctx.surfaceAbs) ?? [],
    whole_surface: surfaceReached ? (ctx.textOf.get(ctx.surfaceAbs) ?? '') : null,
    files,
    globals: inputs.globals,
    pinned: inputs.pinned,
    resolution: inputs.resolution,
    bare_checkout: inputs.bareCheckout,
    // The record fields the check reads (`check.ts`): the pre-gate's
    // findings, and the two marks a probe is built from.
    any_provenance: (record.any_provenance ?? []).map((p) => [p.path, p.kind, p.reason]),
    raw_text_read: record.raw_text_read === true,
    absent_body_tolerated: record.absent_body_tolerated === true,
  };
  return createHash('sha256')
    .update('carrick-surface/1\n')
    .update(JSON.stringify(preimage))
    .digest('hex');
}

/** Resolve a relative specifier from `fromAbs` to a tree file, if present. */
function resolveTreeSpecifier(
  fromAbs: string,
  spec: string,
  tree: Set<string>
): string | undefined {
  if (!isRelative(spec)) return undefined;
  const base = path.resolve(path.dirname(fromAbs), spec);
  return declarationCandidates(base).find((candidate) => tree.has(candidate));
}
