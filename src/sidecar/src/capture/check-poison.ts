/**
 * #438 part 2: stub-tree poison containment.
 *
 * A diagnostic in a service's own stub tree (a broken surface line, a nested
 * declaration file that fails to typecheck) used to poison the WHOLE service —
 * every pair with that service as producer/consumer read `unverifiable`
 * (gate=poison:*), masking every clean pair behind one bad alias. This module
 * contains poison to the aliases actually reachable from the poisoned file:
 *
 *  - a diagnostic on the surface entry is attributed to the alias whose
 *    `export type` statement SPAN covers the diagnostic's line (a multi-line
 *    alias reports on an interior line);
 *  - a diagnostic in a nested tree file is attributed to every alias whose
 *    import closure (surface import-type seeds, then BFS over relative
 *    imports — the same closure the capture self-check computes) includes that
 *    file.
 *
 * A stub-tree diagnostic reachable from NO alias (e.g. a cross-stub
 * `declare global` TS2717 collision in an augmentation file, which
 * `skipLibCheck:false` exists to surface — see check-workspace.ts) has no
 * closure to contain it and genuinely can affect any alias's comparison, so it
 * falls back to service-wide poison. Soundness (never falsely compatible) wins
 * over precision in that one case; every attributable diagnostic is contained.
 *
 * Seam: node builtins + `typescript` + this bundle only.
 */

import ts from 'typescript';
import * as fs from 'node:fs';
import * as path from 'node:path';
import type { AssembledWorkspace } from './check-workspace.js';
import {
  collectSpecifiers,
  declarationCandidates,
  isDeclarationFileName,
  isRelative,
} from './specifiers.js';

export interface StubPoisonIndex {
  serviceName: string;
  /** Workspace-relative surface key: `packages/<dir>/types/surface.d.ts`. */
  surfaceFile: string;
  /** Every alias declared in the surface. */
  allAliases: Set<string>;
  /** Alias whose `export type` statement span covers the given surface line. */
  aliasAtSurfaceLine(line: number): string | undefined;
  /** Aliases whose import closure includes the given workspace-relative file. */
  aliasesForFile(fileKey: string): string[];
}

/** One index per assembled stub, keyed by service name. */
export function buildPoisonIndexes(
  ws: AssembledWorkspace
): Map<string, StubPoisonIndex> {
  const indexes = new Map<string, StubPoisonIndex>();
  for (const stub of ws.stubs) {
    const index = buildOne(ws, stub.serviceName, stub.packageDir);
    if (index) indexes.set(stub.serviceName, index);
  }
  return indexes;
}

function buildOne(
  ws: AssembledWorkspace,
  serviceName: string,
  packageDir: string
): StubPoisonIndex | undefined {
  const typesDirAbs = path.join(ws.workspaceDir, 'packages', packageDir, 'types');
  const surfaceAbs = path.join(typesDirAbs, 'surface.d.ts');
  if (!fs.existsSync(surfaceAbs)) return undefined;

  const keyPrefix = `packages/${packageDir}/types`;
  const surfaceFile = `${keyPrefix}/surface.d.ts`;

  // Collect the whole declaration tree, keyed as `packages/<dir>/types/<rel>`.
  const treeAbs: string[] = [];
  const walk = (dir: string): void => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const p = path.join(dir, entry.name);
      if (entry.isDirectory()) walk(p);
      else if (isDeclarationFileName(entry.name)) treeAbs.push(p);
    }
  };
  walk(typesDirAbs);
  const treeAbsSet = new Set(treeAbs.map((f) => path.resolve(f)));
  const keyOf = (abs: string): string =>
    `${keyPrefix}/${path.relative(typesDirAbs, abs).split(path.sep).join('/')}`;

  const resolveRel = (fromAbs: string, spec: string): string | undefined => {
    if (!isRelative(spec)) return undefined;
    const base = path.resolve(path.dirname(fromAbs), spec);
    return declarationCandidates(base).find((candidate) => treeAbsSet.has(candidate));
  };

  // Per-file relative-import adjacency, over the fileKey namespace.
  const adjacency = new Map<string, string[]>();
  for (const abs of treeAbs) {
    const text = fs.readFileSync(abs, 'utf8');
    const neighbors: string[] = [];
    for (const spec of collectSpecifiers(text)) {
      const target = resolveRel(abs, spec);
      if (target) neighbors.push(keyOf(target));
    }
    adjacency.set(keyOf(abs), neighbors);
  }

  // Surface alias statement spans + each alias's import-type seed files.
  const surfaceText = fs.readFileSync(surfaceAbs, 'utf8');
  const sf = ts.createSourceFile(
    'surface.d.ts',
    surfaceText,
    ts.ScriptTarget.Latest,
    true
  );
  const allAliases = new Set<string>();
  const spans: Array<{ alias: string; startLine: number; endLine: number }> = [];
  const seedsByAlias = new Map<string, string[]>();
  for (const stmt of sf.statements) {
    if (!ts.isTypeAliasDeclaration(stmt)) continue;
    const alias = stmt.name.text;
    allAliases.add(alias);
    spans.push({
      alias,
      startLine: sf.getLineAndCharacterOfPosition(stmt.getStart(sf)).line + 1,
      endLine: sf.getLineAndCharacterOfPosition(stmt.getEnd()).line + 1,
    });
    const seeds: string[] = [];
    const visit = (n: ts.Node): void => {
      if (
        ts.isImportTypeNode(n) &&
        ts.isLiteralTypeNode(n.argument) &&
        ts.isStringLiteral(n.argument.literal) &&
        isRelative(n.argument.literal.text)
      ) {
        const target = resolveRel(surfaceAbs, n.argument.literal.text);
        if (target) seeds.push(keyOf(target));
      }
      n.forEachChild(visit);
    };
    visit(stmt);
    seedsByAlias.set(alias, seeds);
  }

  // Invert per-alias closures into fileKey -> aliases.
  const fileToAliases = new Map<string, Set<string>>();
  for (const [alias, seeds] of seedsByAlias) {
    const closure = new Set<string>();
    const queue = [...seeds];
    while (queue.length > 0) {
      const file = queue.pop()!;
      if (closure.has(file)) continue;
      closure.add(file);
      for (const next of adjacency.get(file) ?? []) queue.push(next);
    }
    for (const file of closure) {
      if (!fileToAliases.has(file)) fileToAliases.set(file, new Set());
      fileToAliases.get(file)!.add(alias);
    }
  }

  return {
    serviceName,
    surfaceFile,
    allAliases,
    aliasAtSurfaceLine(line: number): string | undefined {
      for (const span of spans) {
        if (line >= span.startLine && line <= span.endLine) return span.alias;
      }
      return undefined;
    },
    aliasesForFile(fileKey: string): string[] {
      return [...(fileToAliases.get(fileKey) ?? [])];
    },
  };
}
