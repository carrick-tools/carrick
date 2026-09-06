/**
 * Capture-time self-check (design doc, Capture step 8, amendment 1), with
 * per-alias closure attribution.
 *
 * The stub tree is typechecked standalone with `skipLibCheck: false` --
 * spike-verified as load-bearing: the tree is entirely .d.ts and skipLibCheck
 * skips declaration files wholesale, making the gate vacuous. When the
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
 * file-granularity shortcut is gone.
 */

import ts from 'typescript';
import * as fs from 'node:fs';
import * as path from 'node:path';
import type { CaptureAliasRecord, SelfCheckOutcome } from './api.js';
import type { ResolvedAnchor } from './anchors.js';
import { collectSpecifiers, isRelative, packageNameOf } from './specifiers.js';
import {
  findDisqualifyingTopTypes,
  provenanceOf,
  type DeepTopType,
} from './deep-walk.js';

export interface SelfCheckArgs {
  stubDir: string;
  surfaceAbsPath: string;
  resolved: ResolvedAnchor[];
  pinned: Record<string, string>;
  bareCheckout: boolean;
  /** Producer repo root; its node_modules (if any) backs resolution. */
  repoRoot: string;
}

interface FileFailures {
  externalPinned: Set<string>;
  internal: Set<string>;
}

export function selfCheckStub(args: SelfCheckArgs): CaptureAliasRecord[] {
  const typesDir = path.join(args.stubDir, 'types');
  const treeFiles: string[] = [];
  const walk = (dir: string) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const p = path.join(dir, entry.name);
      if (entry.isDirectory()) walk(p);
      else if (entry.name.endsWith('.d.ts')) treeFiles.push(p);
    }
  };
  walk(typesDir);

  // Point resolution at the producer repo's installed deps when present.
  const repoNodeModules = path.join(args.repoRoot, 'node_modules');
  const linkPath = path.join(args.stubDir, 'node_modules');
  let linked = false;
  if (!args.bareCheckout && fs.existsSync(repoNodeModules) && !fs.existsSync(linkPath)) {
    fs.symlinkSync(repoNodeModules, linkPath, 'dir');
    linked = true;
  }

  try {
    return runSelfCheck(args, treeFiles);
  } finally {
    // unlinkSync, not rmSync: the link target is a directory and rmSync
    // refuses symlinks-to-directories with EISDIR.
    if (linked) fs.unlinkSync(linkPath);
  }
}

function runSelfCheck(args: SelfCheckArgs, treeFiles: string[]): CaptureAliasRecord[] {
  const program = ts.createProgram(treeFiles, {
    noEmit: true,
    strict: true,
    // MUST be false: the whole stub tree is .d.ts (see module header).
    skipLibCheck: false,
    module: ts.ModuleKind.ESNext,
    moduleResolution: ts.ModuleResolutionKind.Bundler,
    types: [],
  });
  const checker = program.getTypeChecker();
  const diagnostics = ts.getPreEmitDiagnostics(program);

  // Failed module specifiers, split external-pinned vs internal, per FILE.
  const failuresByFile = new Map<string, FileFailures>();
  const failuresFor = (fileName: string): FileFailures => {
    let entry = failuresByFile.get(fileName);
    if (!entry) {
      entry = { externalPinned: new Set(), internal: new Set() };
      failuresByFile.set(fileName, entry);
    }
    return entry;
  };
  for (const d of diagnostics) {
    if ((d.code !== 2307 && d.code !== 2792) || !d.file) continue;
    const msg = ts.flattenDiagnosticMessageText(d.messageText, ' ');
    const m = /Cannot find module '([^']+)'/.exec(msg);
    if (!m) continue;
    const spec = m[1];
    const bucket = failuresFor(path.resolve(d.file.fileName));
    if (!isRelative(spec) && args.pinned[packageNameOf(spec)]) {
      bucket.externalPinned.add(spec);
    } else {
      bucket.internal.add(spec);
    }
  }

  // Per-file relative-import adjacency for closure walks.
  const adjacency = new Map<string, string[]>();
  const treeSet = new Set(treeFiles.map((f) => path.resolve(f)));
  for (const file of treeFiles) {
    const abs = path.resolve(file);
    const text = fs.readFileSync(abs, 'utf8');
    const neighbors: string[] = [];
    for (const spec of collectSpecifiers(text)) {
      const resolved = resolveTreeSpecifier(abs, spec, treeSet);
      if (resolved) neighbors.push(resolved);
    }
    adjacency.set(abs, neighbors);
  }

  const surfaceAbs = path.resolve(args.surfaceAbsPath);
  const surfaceSource = program.getSourceFile(surfaceAbs);

  const records: CaptureAliasRecord[] = [];
  for (const anchor of args.resolved) {
    // Demotions (failureReason present) never reached the surface with a
    // real type; everything else — including successful literal anchors,
    // which sit at the structural_fallback TIER but did produce text — is
    // classified by the real self-check.
    records.push(
      anchor.failureReason !== undefined
        ? demotedRecord(anchor)
        : checkedRecord(anchor, {
            args,
            program,
            checker,
            surfaceAbs,
            surfaceSource,
            adjacency,
            failuresByFile,
          })
    );
  }
  return records;
}

/** An alias that never reached a capture-native tier: the failure reason was
 * recorded at demotion time; the surface line is `unknown` by construction. */
function demotedRecord(anchor: ResolvedAnchor): CaptureAliasRecord {
  return {
    alias: anchor.request.alias,
    anchor_kind: anchor.request.kind,
    symbol_name: 'symbol_name' in anchor.request ? anchor.request.symbol_name : undefined,
    source_file: 'source_file' in anchor.request ? anchor.request.source_file : '<inline>',
    anchor_origin: anchor.request.anchor_origin,
    serialization: 'structural_fallback',
    self_check: 'decayed_internal',
    self_check_detail: anchor.failureReason,
    capture_failure_reason: anchor.failureReason,
    top_type_at_self_check: true,
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
  }
): CaptureAliasRecord {
  const alias = anchor.request.alias;
  let topType = true;
  let deepFindings: DeepTopType[] = [];
  const seeds: string[] = [];

  if (ctx.surfaceSource) {
    for (const stmt of ctx.surfaceSource.statements) {
      if (!ts.isTypeAliasDeclaration(stmt) || stmt.name.text !== alias) continue;
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
  for (const file of closure) {
    const failures = ctx.failuresByFile.get(file);
    if (!failures) continue;
    if (!blamedExternal) blamedExternal = [...failures.externalPinned][0];
    if (!internalFailure) internalFailure = [...failures.internal][0];
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
        detail = `unresolved pinned external '${blamedExternal}' on an installed checkout`;
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
      detail = `unresolved pinned external '${blamedExternal}' on an installed checkout`;
    }
  }

  return {
    alias,
    anchor_kind: anchor.request.kind,
    symbol_name: 'symbol_name' in anchor.request ? anchor.request.symbol_name : undefined,
    source_file: 'source_file' in anchor.request ? anchor.request.source_file : '<inline>',
    anchor_origin: anchor.request.anchor_origin,
    serialization: anchor.serialization,
    self_check: outcome,
    // A #438/#439 re-aim note is provenance only; a real decay detail always
    // wins, so it surfaces exactly when the re-aimed alias self-checks clean.
    self_check_detail: detail ?? anchor.reaimNote,
    top_type_at_self_check: topType,
    ...(unexplainedDeep.length > 0
      ? { any_provenance: unexplainedDeep.map(provenanceOf) }
      : {}),
  };
}

/** Resolve a relative specifier from `fromAbs` to a tree file, if present. */
function resolveTreeSpecifier(
  fromAbs: string,
  spec: string,
  tree: Set<string>
): string | undefined {
  if (!isRelative(spec)) return undefined;
  const base = path.resolve(path.dirname(fromAbs), spec);
  const candidates = [
    `${base}.d.ts`,
    path.join(base, 'index.d.ts'),
    base.endsWith('.js') ? `${base.slice(0, -3)}.d.ts` : undefined,
    base, // already .d.ts
  ].filter((c): c is string => c !== undefined);
  for (const candidate of candidates) {
    if (tree.has(candidate)) return candidate;
  }
  return undefined;
}
