/**
 * Repair an emitted declaration whose own import does not resolve
 * (carrick#1397).
 *
 * A `symbol` anchor prints nothing: its surface line is
 * `import('./m').RenderRequest` and the shape lives in the `.d.ts` the
 * compiler emitted for `m`. When that file imports a module the scanned
 * checkout does not have — a dependency that was not installed, a generated
 * module that was never generated — the stub reports the module missing, the
 * self-check records a dangling internal specifier, and the scanner's publish
 * gate refuses the alias whole. Twenty resolved members are thrown away with
 * the one that did not resolve, which is exactly the loss carrick#1377 named
 * for the two PRINT paths.
 *
 * So the declaration is repaired: the import that did not resolve is dropped
 * and every type it bound is written `unknown` at its own position. This is
 * file-granular — the same emitted file backs every alias whose closure
 * reaches it — and it says nothing the source program did not already say:
 * those names were TypeScript's unresolved-reference placeholder there too,
 * which is why the anchor recorded their member paths as unresolved.
 *
 * Fail-closed in two places. A name used where `unknown` is not a type — a
 * heritage clause, a type-parameter position — leaves the file untouched, so
 * the alias keeps its honest refusal rather than gaining a broken declaration.
 * And the caller re-checks the repaired tree: a name the rewrite did not reach
 * turns into a `Cannot find name` diagnostic, which the self-check reads back
 * as the same dangling specifier.
 *
 * Seam: node builtins + `typescript`, like the rest of this directory.
 */

import ts from 'typescript';
import * as fs from 'node:fs';

/** What one file's repair removed, for the caller's re-check. */
export interface RepairedFile {
  /** Specifiers whose import statements were dropped. */
  specifiers: string[];
  /** Names those imports bound, now written `unknown` wherever they were used. */
  names: string[];
}

interface Edit {
  start: number;
  end: number;
  text: string;
}

/**
 * Drop every import of a failing specifier from each file and write `unknown`
 * where its names were used.
 *
 * `failing` maps an absolute file path to the specifiers the stub could not
 * resolve from it. Returns the files actually rewritten; a file whose names
 * cannot all be replaced by `unknown` is left exactly as it was.
 */
export function repairDanglingImports(
  failing: Map<string, Set<string>>
): Map<string, RepairedFile> {
  const repaired = new Map<string, RepairedFile>();
  for (const [file, specifiers] of failing) {
    const result = repairFile(file, specifiers);
    if (result) repaired.set(file, result);
  }
  return repaired;
}

function repairFile(file: string, specifiers: Set<string>): RepairedFile | undefined {
  let text: string;
  try {
    text = fs.readFileSync(file, 'utf8');
  } catch {
    return undefined;
  }
  const source = ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);

  const names = new Set<string>();
  const edits: Edit[] = [];
  const removedSpecifiers = new Set<string>();

  // A re-export of the failing module binds nothing in THIS file and is what
  // another file reads through: dropping it would turn "cannot find module"
  // into "has no exported member" over there, which is neither diagnostic the
  // re-check reads, and the importer's member would publish as clean. There is
  // no `unknown` to write for an export, so the file keeps its refusal.
  let unreplaceable = false;

  for (const statement of source.statements) {
    const specifier = importSpecifierOf(statement);
    if (specifier === undefined || !specifiers.has(specifier)) continue;
    if (ts.isExportDeclaration(statement)) {
      unreplaceable = true;
      break;
    }
    for (const name of boundNames(statement)) names.add(name);
    removedSpecifiers.add(specifier);
    edits.push({ start: statement.getFullStart(), end: statement.getEnd(), text: '' });
  }

  // An `import(...)` type written inline carries its own specifier and has no
  // statement to drop; the whole node becomes `unknown`.
  const visit = (node: ts.Node): void => {
    if (ts.isImportTypeNode(node) && ts.isLiteralTypeNode(node.argument)) {
      const literal = node.argument.literal;
      if (ts.isStringLiteral(literal) && specifiers.has(literal.text)) {
        removedSpecifiers.add(literal.text);
        edits.push({ start: node.getStart(source), end: node.getEnd(), text: 'unknown' });
        return;
      }
    }
    if (ts.isTypeReferenceNode(node) && names.has(leftmostName(node.typeName))) {
      edits.push({ start: node.getStart(source), end: node.getEnd(), text: 'unknown' });
      return;
    }
    if (ts.isTypeQueryNode(node) && names.has(leftmostName(node.exprName))) {
      edits.push({ start: node.getStart(source), end: node.getEnd(), text: 'unknown' });
      return;
    }
    // Positions where `unknown` is not a type: what a declaration EXTENDS or
    // IMPLEMENTS, and a type parameter's own name. Leave the file alone rather
    // than emit a declaration that does not parse as it used to read.
    if (
      ts.isExpressionWithTypeArguments(node) &&
      ts.isIdentifier(node.expression) &&
      names.has(node.expression.text)
    ) {
      unreplaceable = true;
      return;
    }
    node.forEachChild(visit);
  };
  visit(source);

  if (unreplaceable || removedSpecifiers.size === 0) return undefined;

  edits.sort((a, b) => b.start - a.start);
  let out = text;
  for (const edit of edits) {
    out = out.slice(0, edit.start) + edit.text + out.slice(edit.end);
  }
  fs.writeFileSync(file, out);
  return { specifiers: [...removedSpecifiers].sort(), names: [...names].sort() };
}

/** The module specifier an import STATEMENT names, if it names one. */
function importSpecifierOf(statement: ts.Statement): string | undefined {
  let expression: ts.Expression | undefined;
  if (ts.isImportDeclaration(statement) || ts.isExportDeclaration(statement)) {
    expression = statement.moduleSpecifier;
  } else if (
    ts.isImportEqualsDeclaration(statement) &&
    ts.isExternalModuleReference(statement.moduleReference)
  ) {
    expression = statement.moduleReference.expression;
  }
  return expression && ts.isStringLiteral(expression) ? expression.text : undefined;
}

/** Every local name an import statement binds. */
function boundNames(statement: ts.Statement): string[] {
  const out: string[] = [];
  if (ts.isImportDeclaration(statement)) {
    const clause = statement.importClause;
    if (!clause) return out;
    if (clause.name) out.push(clause.name.text);
    const bindings = clause.namedBindings;
    if (bindings && ts.isNamespaceImport(bindings)) out.push(bindings.name.text);
    if (bindings && ts.isNamedImports(bindings)) {
      for (const element of bindings.elements) out.push(element.name.text);
    }
  } else if (ts.isImportEqualsDeclaration(statement)) {
    out.push(statement.name.text);
  }
  return out;
}

/** The leftmost identifier of `A`, `A.B` or `A.B.C`. */
function leftmostName(name: ts.EntityName | ts.QualifiedName): string {
  let current: ts.EntityName = name;
  while (ts.isQualifiedName(current)) current = current.left;
  return current.text;
}
