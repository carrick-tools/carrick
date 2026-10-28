/**
 * No stub names a module by a machine path (carrick#2943).
 *
 * The specifier rewrite maps an absolute specifier onto a file of the stub's
 * tree or onto an installed package. What it cannot map stays as printed: a
 * type that a printer could name only by where its module sits on this
 * machine, such as a workspace sibling's source reached through a
 * `node_modules` link, which the declaration emit does not carry. That path
 * resolves on the machine that ran the capture, to a source file outside the
 * stub, and nowhere else: the upload rewrites it to `<checkout>/...`, so the
 * stored stub and a fresh one disagree about the same alias.
 *
 * Each such reference becomes `unknown` at its own position. In the surface
 * the position is reported per alias, so the self-check labels the finding
 * there as a reference that did not resolve rather than as declared; the rest
 * of the alias's type survives. Any other tree file gets the dangling-import
 * repair (carrick#1397), which writes `unknown` where the import's names were
 * used. The stub then reads the same on every machine.
 *
 * Seam: node builtins + `typescript`, like the rest of this directory.
 */

import ts from 'typescript';
import * as fs from 'node:fs';
import * as path from 'node:path';
import type { WriteGuard } from './guarded-fs.js';
import { childPath } from './node-builder.js';
import { repairDanglingImports } from './repair-dangling.js';
import { collectSpecifiers } from './specifiers.js';

/** A specifier that names a module by where it sits on one machine. */
export function isMachinePath(specifier: string): boolean {
  return path.posix.isAbsolute(specifier) || /^[A-Za-z]:[\\/]/.test(specifier) || specifier.startsWith('\\\\');
}

/** What the surface substitution replaced in one alias. */
export interface MachinePathPositions {
  /** Member paths now `unknown`, in the deep walk's notation (`''` = root). */
  paths: string[];
  /** The modules they named, relative to `labelRoot` (never absolute). */
  modules: string[];
}

/**
 * Replace every machine-path import in the stub's tree with `unknown`.
 * `files` are absolute tree paths; `surfaceAbsPath` is one of them. Returns
 * the positions replaced in the surface, by alias.
 */
export function substituteMachinePathImports(args: {
  files: readonly string[];
  surfaceAbsPath: string;
  /** A guard over the stub's types tree. */
  tree: WriteGuard;
  /** The root a module is named from in a label (the scanned repo's). */
  labelRoot: string;
}): Map<string, MachinePathPositions> {
  const failing = new Map<string, Set<string>>();
  for (const file of args.files) {
    if (path.resolve(file) === path.resolve(args.surfaceAbsPath)) continue;
    const machine = [...collectSpecifiers(fs.readFileSync(file, 'utf8'))].filter(isMachinePath);
    if (machine.length > 0) failing.set(file, new Set(machine));
  }
  if (failing.size > 0) repairDanglingImports(failing, args.tree);
  return substituteInSurface(args.surfaceAbsPath, args.tree, args.labelRoot);
}

function substituteInSurface(
  surfaceAbsPath: string,
  tree: WriteGuard,
  labelRoot: string
): Map<string, MachinePathPositions> {
  const byAlias = new Map<string, MachinePathPositions>();
  const text = fs.readFileSync(surfaceAbsPath, 'utf8');
  if (!/import\s*\(/.test(text)) return byAlias;
  const source = ts.createSourceFile(surfaceAbsPath, text, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
  const edits: Array<{ start: number; end: number }> = [];
  for (const statement of source.statements) {
    if (!ts.isTypeAliasDeclaration(statement)) continue;
    const found: MachinePathPositions = { paths: [], modules: [] };
    const visit = (node: ts.Node, at: string): void => {
      const specifier = importTypeSpecifier(node);
      if (specifier !== undefined && isMachinePath(specifier)) {
        edits.push({ start: node.getStart(source), end: node.getEnd() });
        if (!found.paths.includes(at)) found.paths.push(at);
        const module = moduleLabel(specifier, labelRoot);
        if (!found.modules.includes(module)) found.modules.push(module);
        return;
      }
      node.forEachChild((child) => visit(child, childPath(node, child, at)));
    };
    visit(statement.type, '');
    if (found.paths.length > 0) byAlias.set(statement.name.text, found);
  }
  if (edits.length === 0) return byAlias;
  edits.sort((a, b) => b.start - a.start);
  let out = text;
  for (const edit of edits) out = `${out.slice(0, edit.start)}unknown${out.slice(edit.end)}`;
  tree.writeFile(surfaceAbsPath, out);
  return byAlias;
}

/** The string an `import("...")` type names, when `node` is one. */
function importTypeSpecifier(node: ts.Node): string | undefined {
  if (!ts.isImportTypeNode(node) || !ts.isLiteralTypeNode(node.argument)) return undefined;
  const literal = node.argument.literal;
  return ts.isStringLiteral(literal) ? literal.text : undefined;
}

/** `specifier` relative to `root`, with forward slashes. */
function moduleLabel(specifier: string, root: string): string {
  return path.relative(path.resolve(root), path.resolve(specifier)).split(path.sep).join('/');
}
