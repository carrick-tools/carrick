/**
 * Which files an edit can reach (carrick#2881, carrick#3100).
 *
 * A PR check re-types only the aliases its change can reach. This module says
 * where: the changed files, and every file whose types read a declaration the
 * edit changed, followed name by name rather than file by file, so a barrel
 * (`index.ts` with `export *`) does not turn one changed export into every
 * importer of the package.
 *
 * The walk:
 * - In a changed file, a top-level name is changed when the text of a
 *   statement declaring it changed, or when a statement declaring it mentions
 *   a changed name of the same file (so `type Dto = z.infer<typeof schema>`
 *   changes with `schema`). A statement that declares nothing (an expression
 *   statement, which can add an expando member to a function) changes every
 *   name it mentions when it changes or mentions a changed name. In a
 *   CommonJS file, a changed statement that mentions `module` or `exports`
 *   changes the whole module.
 * - An importing file is reached when one of its imports resolves, through
 *   every re-export (the compiler's own alias resolution), to a changed
 *   declaration. A namespace import reads every export of its module, and so
 *   does a dynamic `import()`, `require()` or `import('m').T`, read as a
 *   binding of the statement that holds it. Its names that
 *   mention the reached imports change in turn, and the walk goes on to their
 *   importers. A name whose declaration mentions no changed name stops the walk
 *   there, whatever its printed type says.
 * - An added or deleted file is an empty module (`export {};`) on its missing
 *   side. It changes every name of the file, and its importers are reached
 *   through the graph. Its export names are not matched against importers
 *   elsewhere in the repo: nothing imports a name from a file it did not
 *   resolve to. What it can change is where an import is looked up, so every
 *   import records the stem it was looked up at (a relative specifier's path,
 *   and the repo file it resolved to, without extension and trailing
 *   `/index`), and an added or deleted file whose own stem matches reaches
 *   that import's file.
 * - A global declaration or a module augmentation (`declare global`, `declare
 *   module 'x'`) is read by files that never import it (a value returned by
 *   the augmented package carries it), so a change to one, or to a name one
 *   mentions, is returned in `augmented`: the caller widens it to every file
 *   of the programs that hold it, as TypeScript's own scope for an
 *   augmentation does. So is any change to a script (a file with no import or
 *   export, all of it global). A file that is not TypeScript or JavaScript
 *   source, or an installed package's file, reaches every file. An import that
 *   resolves to an augmented interface reads the augmentation by its own name
 *   (`AUGMENTATION`), so an unchanged augmentation stops nothing.
 * - An export added or removed in a file that exists on both sides (an export
 *   list, or a new exported declaration) reaches every file importing that
 *   name from anywhere, and the statements of every file holding a namespace
 *   or dynamic import that read it.
 *
 * Dependencies run from a file to the declarations it names; the compiler
 * never types a file from the files that import it, so nothing is followed the
 * other way. One assumption: an installed package's declarations do not import
 * the workspace's own sources (only a workspace package could, and its files
 * are read at their real path, outside `node_modules`).
 *
 * The graph is read from a program's own checker, once per program, and keyed
 * by path relative to a root. `serialiseGraph` writes it without statement
 * texts; `reach` walks that form, so a caller needs no program.
 */

import * as path from 'node:path';
import { ts } from 'ts-morph';
import type { SerialGraph } from './types.js';

/** A top-level declaration a file's import reads: `name` `*` is the whole file. */
export interface Origin {
  file: string;
  name: string;
}

/** The top-level statements of one file, read from its text alone. */
export interface Shape {
  /** Each top-level name, with the texts of the statements declaring it, in order. */
  declared: Map<string, string[]>;
  /** Each top-level name, with the identifiers its statements mention. */
  refs: Map<string, Set<string>>;
  /** Statements that declare nothing: where each starts, its text and the identifiers it mentions. */
  loose: Array<{ pos: number; text: string; refs: Set<string> }>;
  /** The names importers can import from it. */
  exports: Set<string>;
  /**
   * Each name an export list or `export default x` gives a local: an importer
   * of the name resolves to the local's declaration, so pointing the name at
   * another local moves it as adding it would.
   */
  exportTargets: Map<string, string>;
  /** It is a script: all of it is global, and a change reaches every file. */
  global: boolean;
}

/** One file of the graph: its shape and where each of its imports leads. */
interface FileNode {
  shape: Shape;
  /** Each import binding, by local name: the declarations it resolves to. */
  bindings: Map<string, Origin[]>;
  /** Each named import binding, by local name: the name it imports. */
  importedNames: Map<string, string>;
  /**
   * The bindings that read every export of a module: a namespace import, an
   * `import x = require()`, and each dynamic `import()`, `require()` or
   * `import('m').T`, bound under a name no identifier can spell and mentioned
   * by the statement that holds it.
   */
  wholeLocals: Set<string>;
  /** Where each import was looked up: an added or deleted file at that stem changes what it resolves to. */
  stems: StemEntry[];
}

/** One import specifier of a file, and the stems it was looked up at. */
export interface StemEntry {
  /** The stems: the relative specifier's path, and the repo file it resolved to. */
  stems: string[];
  /** The repo file it resolved to, when it resolved to one. */
  target?: string;
  /** The bindings the specifier introduces. */
  locals: string[];
}

/** The import graph of one program. */
export interface ImportGraph {
  nodes: Map<string, FileNode>;
  /** Each file, with the files whose imports resolve into it. */
  importers: Map<string, Set<string>>;
}

/** What an edit changed in one file: its text before and after. */
export interface FileChange {
  file: string;
  before: string;
  after: string;
}

const SOURCE_EXTENSION = /\.(?:[cm]?[jt]sx?)$/i;

/** The source a missing side of an added or deleted file reads as. */
const EMPTY_MODULE = 'export {};';

/** `a/b.d.ts`, `a/b.js` and `a/b/index.ts` all look up as `a/b`. */
export function stemOf(file: string): string {
  const stem = file
    .replace(/\.d\.[cm]?ts$/i, '')
    .replace(SOURCE_EXTENSION, '')
    .replace(/(?:^|\/)index$/, '');
  return stem;
}

function isInstalled(file: string): boolean {
  // A path relative to the root starts inside `node_modules` without a slash before it.
  return `/${file}`.includes('/node_modules/');
}

function hasModifier(node: ts.Node, kind: ts.SyntaxKind): boolean {
  return (ts.canHaveModifiers(node) ? ts.getModifiers(node) : undefined)?.some((m) => m.kind === kind) ?? false;
}

function bindingNames(name: ts.BindingName, into: string[]): void {
  if (ts.isIdentifier(name)) {
    into.push(name.text);
    return;
  }
  for (const element of name.elements) {
    if (!ts.isOmittedExpression(element)) bindingNames(element.name, into);
  }
}

/**
 * The name a global declaration or module augmentation is known by: no
 * identifier can spell it, so only an import resolving into it names it.
 */
const AUGMENTATION = '\u0000augments ';

function isAugmentation(name: string): boolean {
  return name.startsWith(AUGMENTATION);
}

/** The top-level names one statement declares. */
export function declaredNames(statement: ts.Statement): string[] {
  const names: string[] = [];
  if (ts.isVariableStatement(statement)) {
    for (const decl of statement.declarationList.declarations) bindingNames(decl.name, names);
  } else if (
    ts.isFunctionDeclaration(statement) ||
    ts.isClassDeclaration(statement) ||
    ts.isInterfaceDeclaration(statement) ||
    ts.isTypeAliasDeclaration(statement) ||
    ts.isEnumDeclaration(statement)
  ) {
    if (statement.name) names.push(statement.name.text);
    if (hasModifier(statement, ts.SyntaxKind.DefaultKeyword)) names.push('default');
  } else if (ts.isModuleDeclaration(statement)) {
    const global = ts.isIdentifier(statement.name) && statement.name.text === 'global';
    names.push(ts.isIdentifier(statement.name) && !global ? statement.name.text : `${AUGMENTATION}${statement.name.text}`);
  } else if (ts.isImportDeclaration(statement)) {
    const clause = statement.importClause;
    if (clause?.name) names.push(clause.name.text);
    const bindings = clause?.namedBindings;
    if (bindings && ts.isNamespaceImport(bindings)) names.push(bindings.name.text);
    if (bindings && ts.isNamedImports(bindings)) for (const el of bindings.elements) names.push(el.name.text);
  } else if (ts.isImportEqualsDeclaration(statement)) {
    names.push(statement.name.text);
  } else if (ts.isExportAssignment(statement)) {
    names.push(statement.isExportEquals ? 'export=' : 'default');
  }
  return names;
}

const JAVASCRIPT = /\.[cm]?jsx?$/i;

/**
 * Visit `n`'s children and, in a JavaScript file, the JSDoc comments attached
 * to it first: there `@type`, `@param`, `@returns`, `@typedef` and `@import`
 * type the file, and `forEachChild` never enters them.
 */
function forEachChildAndJSDoc(n: ts.Node, js: boolean, visit: (child: ts.Node) => void): void {
  if (js) for (const doc of (n as { jsDoc?: ts.JSDoc[] }).jsDoc ?? []) visit(doc);
  n.forEachChild(visit);
}

/** The type names a JavaScript statement's JSDoc declares (`@typedef`, `@callback`): top-level names of the file. */
function jsDocNames(statement: ts.Node, js: boolean): string[] {
  if (!js) return [];
  const names: string[] = [];
  for (const doc of (statement as { jsDoc?: ts.JSDoc[] }).jsDoc ?? []) {
    for (const tag of doc.tags ?? []) {
      const name = typedefName(tag);
      if (name !== undefined) names.push(name);
    }
  }
  return names;
}

function typedefName(node: ts.Node): string | undefined {
  if (!ts.isJSDocTypedefTag(node) && !ts.isJSDocCallbackTag(node)) return undefined;
  const name = node.name;
  return name && ts.isIdentifier(name) ? name.text : undefined;
}

/**
 * The statements of a file, and in a JavaScript file the end-of-file token
 * when a JSDoc comment (a trailing `@typedef`) is attached to it.
 */
function statementsOf(sourceFile: ts.SourceFile, js: boolean): ts.Node[] {
  const statements: ts.Node[] = [...sourceFile.statements];
  if (js && ((sourceFile.endOfFileToken as { jsDoc?: ts.JSDoc[] }).jsDoc ?? []).length > 0) {
    statements.push(sourceFile.endOfFileToken);
  }
  return statements;
}

function mentioned(node: ts.Node, js: boolean): Set<string> {
  const refs = new Set<string>();
  const visit = (n: ts.Node): void => {
    if (ts.isIdentifier(n)) refs.add(n.text);
    forEachChildAndJSDoc(n, js, visit);
  };
  visit(node);
  return refs;
}

function scriptKindOf(file: string): ts.ScriptKind {
  const ext = file.slice(file.lastIndexOf('.')).toLowerCase();
  if (ext === '.tsx') return ts.ScriptKind.TSX;
  if (ext === '.jsx') return ts.ScriptKind.JSX;
  if (ext === '.js' || ext === '.mjs' || ext === '.cjs') return ts.ScriptKind.JS;
  return ts.ScriptKind.TS;
}

/** JavaScript text that reads as a CommonJS module. */
const COMMONJS_HINT = /\brequire\s*\(|\bmodule\.exports\b|\bexports\.[A-Za-z_$]/;

/**
 * Whether text alone reads as a module. In TypeScript that is an import or an
 * export. A JavaScript file is also a module when it uses CommonJS, and a
 * `.mjs` file always is. The program reads a file by its own rules; when the
 * two readings disagree the caller reads the file as a script.
 */
function textIsModule(sourceFile: ts.SourceFile): boolean {
  if (ts.isExternalModule(sourceFile)) return true;
  const file = sourceFile.fileName;
  if (!JAVASCRIPT.test(file)) return false;
  return /\.mjs$/i.test(file) || COMMONJS_HINT.test(sourceFile.text);
}

/** A file's shape from its text. */
export function shapeOf(file: string, text: string): Shape {
  const sourceFile = ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true, scriptKindOf(file));
  return shapeOfSource(sourceFile, !textIsModule(sourceFile));
}

function shapeOfSource(sourceFile: ts.SourceFile, global: boolean): Shape {
  const shape: Shape = {
    declared: new Map(),
    refs: new Map(),
    loose: [],
    exports: new Set(),
    exportTargets: new Map(),
    global,
  };
  const js = JAVASCRIPT.test(sourceFile.fileName);
  const declare = (name: string, text: string, refs: Set<string>): void => {
    shape.declared.set(name, [...(shape.declared.get(name) ?? []), text]);
    const known = shape.refs.get(name) ?? new Set<string>();
    for (const ref of refs) known.add(ref);
    shape.refs.set(name, known);
  };
  for (const node of statementsOf(sourceFile, js)) {
    // With its leading comments: a JSDoc comment types a JavaScript file.
    const text = node.getFullText(sourceFile);
    // A top-level `@typedef` is a name of the file, exported from a module.
    const typedefs = jsDocNames(node, js);
    if (typedefs.length > 0) {
      const refs = mentioned(node, js);
      for (const name of typedefs) {
        declare(name, text, refs);
        shape.exports.add(name);
      }
    }
    if (node.kind === ts.SyntaxKind.EndOfFileToken) {
      shape.loose.push({ pos: node.pos, text, refs: mentioned(node, js) });
      continue;
    }
    const statement = node as ts.Statement;
    if (ts.isExportDeclaration(statement)) {
      const clause = statement.exportClause;
      if (!clause) {
        // `export * from`: which names it brings is the target's; adding or
        // removing one is a structure change the freshness sweep rebuilds on.
        shape.exports.add(`*${statement.moduleSpecifier?.getText(sourceFile) ?? ''}`);
      } else if (ts.isNamespaceExport(clause)) {
        shape.exports.add(clause.name.text);
      } else {
        for (const el of clause.elements) {
          shape.exports.add(el.name.text);
          if (!statement.moduleSpecifier) shape.exportTargets.set(el.name.text, (el.propertyName ?? el.name).text);
          // `export { a as b }` of a local: `b` reads `a`.
          if (!statement.moduleSpecifier && el.propertyName) declare(el.name.text, text, new Set([el.propertyName.text]));
        }
      }
      if (statement.moduleSpecifier) shape.loose.push({ pos: statement.pos, text, refs: new Set() });
      continue;
    }
    const names = declaredNames(statement);
    if (names.length === 0) {
      shape.loose.push({ pos: statement.pos, text, refs: mentioned(statement, js) });
      continue;
    }
    const refs = ts.isImportDeclaration(statement) || ts.isImportEqualsDeclaration(statement)
      ? new Set<string>()
      : mentioned(statement, js);
    for (const name of names) declare(name, text, refs);
    if (hasModifier(statement, ts.SyntaxKind.ExportKeyword) || ts.isExportAssignment(statement)) {
      for (const name of names) shape.exports.add(name);
    }
    if (ts.isExportAssignment(statement) && ts.isIdentifier(statement.expression)) {
      shape.exportTargets.set(names[0]!, statement.expression.text);
    }
  }
  return shape;
}

/** The names of `shape` that read any of `seeds`, the seeds included. */
export function closeOver(shape: Shape, seeds: Iterable<string>): Set<string> {
  const changed = new Set(seeds);
  let grew = true;
  while (grew) {
    grew = false;
    for (const [name, refs] of shape.refs) {
      if (changed.has(name)) continue;
      for (const ref of refs) {
        if (changed.has(ref)) {
          changed.add(name);
          grew = true;
          break;
        }
      }
    }
    for (const loose of shape.loose) {
      if (![...loose.refs].some((ref) => changed.has(ref))) continue;
      for (const ref of loose.refs) {
        if (shape.declared.has(ref) && !changed.has(ref)) {
          changed.add(ref);
          grew = true;
        }
      }
    }
  }
  return changed;
}

/** The top-level names whose statements differ between two shapes of one file. */
function changedNames(file: string, before: Shape, after: Shape): Set<string> {
  const seeds = new Set<string>();
  for (const name of new Set([...before.declared.keys(), ...after.declared.keys()])) {
    const a = before.declared.get(name) ?? [];
    const b = after.declared.get(name) ?? [];
    if (a.length !== b.length || a.some((text, i) => text !== b[i])) seeds.add(name);
  }
  // A statement that declares nothing and is not in both: every name it
  // mentions may read differently.
  const count = (list: Shape['loose']) => {
    const counts = new Map<string, number>();
    for (const { text } of list) counts.set(text, (counts.get(text) ?? 0) + 1);
    return counts;
  };
  const was = count(before.loose);
  const now = count(after.loose);
  for (const [shape, other] of [
    [before, now],
    [after, was],
  ] as const) {
    for (const loose of shape.loose) {
      if ((other.get(loose.text) ?? 0) > 0) continue;
      for (const ref of loose.refs) if (before.declared.has(ref) || after.declared.has(ref)) seeds.add(ref);
      // `module.exports = x` and `exports.a = x` give a CommonJS module its exports.
      if (JAVASCRIPT.test(file) && (loose.refs.has('module') || loose.refs.has('exports'))) seeds.add('*');
    }
  }
  const merged: Shape = {
    declared: new Map([...before.declared, ...after.declared]),
    refs: new Map<string, Set<string>>(),
    loose: [...before.loose, ...after.loose],
    exports: new Set(),
    exportTargets: new Map(),
    global: false,
  };
  for (const source of [before.refs, after.refs]) {
    for (const [name, refs] of source) {
      const known = merged.refs.get(name) ?? new Set<string>();
      for (const ref of refs) known.add(ref);
      merged.refs.set(name, known);
    }
  }
  return closeOver(merged, seeds);
}

function topStatement(node: ts.Node): ts.Statement | undefined {
  let at: ts.Node = node;
  while (at.parent && !ts.isSourceFile(at.parent)) at = at.parent;
  return at.parent && ts.isSourceFile(at.parent) ? (at as ts.Statement) : undefined;
}

function originsOfSymbol(checker: ts.TypeChecker, symbol: ts.Symbol | undefined, into: Origin[]): void {
  if (!symbol) return;
  const target = symbol.flags & ts.SymbolFlags.Alias ? safeAliased(checker, symbol) : symbol;
  for (const decl of target?.declarations ?? []) {
    const sourceFile = decl.getSourceFile();
    if (ts.isSourceFile(decl)) {
      into.push({ file: sourceFile.fileName, name: '*' });
      continue;
    }
    const typedef = typedefName(decl);
    if (typedef !== undefined) {
      into.push({ file: sourceFile.fileName, name: typedef });
      continue;
    }
    const top = topStatement(decl);
    const names = top ? declaredNames(top) : [];
    if (names.length === 0) into.push({ file: sourceFile.fileName, name: '*' });
    for (const name of names) into.push({ file: sourceFile.fileName, name });
  }
}

function safeAliased(checker: ts.TypeChecker, symbol: ts.Symbol): ts.Symbol | undefined {
  try {
    return checker.getAliasedSymbol(symbol);
  } catch {
    return undefined;
  }
}

/** Every declaration a module's exports resolve to, and the module file itself when it exports `=`. */
function originsOfModule(checker: ts.TypeChecker, moduleSymbol: ts.Symbol | undefined, into: Origin[]): void {
  if (!moduleSymbol) return;
  const target = moduleSymbol.flags & ts.SymbolFlags.Alias ? safeAliased(checker, moduleSymbol) : moduleSymbol;
  if (!target) return;
  for (const decl of target.declarations ?? []) into.push({ file: decl.getSourceFile().fileName, name: '*' });
  let exports: ts.Symbol[] = [];
  try {
    exports = checker.getExportsOfModule(target);
  } catch {
    exports = [];
  }
  for (const exported of exports) originsOfSymbol(checker, exported, into);
}

const WHOLE_MODULE_HINT = /\bimport\s*\(|\brequire\s*\(/;

/** A JavaScript file's `@import` tag. */
const JSDOC_IMPORT_HINT = /@import\b/;

/** The name a dynamic import, `require()` or import type is bound under: no identifier can spell it. */
const WHOLE_MODULE = '\u0000reads module ';

/**
 * Whether the program reads a file as a module: an ES module, or CommonJS
 * JavaScript (`module.exports`, `exports.a`). The checker's own reading, which
 * the typings do not list; it is set once the program is bound.
 */
function isModuleInProgram(sourceFile: ts.SourceFile): boolean {
  return (ts as unknown as { isExternalOrCommonJsModule(file: ts.SourceFile): boolean }).isExternalOrCommonJsModule(
    sourceFile
  );
}

/** A path relative to `root`, with `/` separators. */
function relativeTo(root: string, file: string): string {
  return path.relative(root, file).split(path.sep).join('/');
}

/** A repo file's path relative to `root`; `undefined` for an installed file. */
function repoPath(root: string, file: string): string | undefined {
  return isInstalled(file) ? undefined : relativeTo(root, file);
}

function nodeOf(checker: ts.TypeChecker, root: string, sourceFile: ts.SourceFile): FileNode {
  const file = relativeTo(root, sourceFile.fileName);
  const node: FileNode = {
    shape: shapeOfSource(sourceFile, !isModuleInProgram(sourceFile)),
    bindings: new Map(),
    importedNames: new Map(),
    wholeLocals: new Set(),
    stems: [],
  };
  /** The origins a binding resolves to: repo files only, relative to the root. */
  const setBinding = (local: string, origins: Origin[]): void => {
    const seen = new Set<string>();
    const kept: Origin[] = [];
    for (const origin of origins) {
      const at = repoPath(root, origin.file);
      if (at === undefined || seen.has(`${at}\0${origin.name}`)) continue;
      seen.add(`${at}\0${origin.name}`);
      kept.push({ file: at, name: origin.name });
    }
    node.bindings.set(local, kept);
  };
  /** Records where a specifier was looked up, and the repo file it resolved to. */
  const lookUp = (specifier: ts.Expression, locals: string[]): void => {
    if (!ts.isStringLiteralLike(specifier)) return;
    const stems: string[] = [];
    if (specifier.text.startsWith('.')) {
      stems.push(stemOf(path.posix.normalize(path.posix.join(path.posix.dirname(file), specifier.text))));
    }
    let target: string | undefined;
    const declaration = checker.getSymbolAtLocation(specifier)?.declarations?.[0];
    if (declaration) target = repoPath(root, declaration.getSourceFile().fileName);
    if (target !== undefined) stems.push(stemOf(target));
    if (stems.length === 0) return;
    node.stems.push({ stems: [...new Set(stems)], ...(target === undefined ? {} : { target }), locals });
  };
  const bind = (local: ts.Identifier, imported: string | undefined): void => {
    const origins: Origin[] = [];
    originsOfSymbol(checker, checker.getSymbolAtLocation(local), origins);
    setBinding(local.text, origins);
    if (imported !== undefined) node.importedNames.set(local.text, imported);
  };
  for (const statement of sourceFile.statements) {
    if (ts.isImportDeclaration(statement)) {
      const clause = statement.importClause;
      if (!clause) {
        lookUp(statement.moduleSpecifier, []);
        continue;
      }
      if (clause.name) bind(clause.name, 'default');
      const bindings = clause.namedBindings;
      if (bindings && ts.isNamespaceImport(bindings)) {
        const origins: Origin[] = [];
        originsOfModule(checker, checker.getSymbolAtLocation(statement.moduleSpecifier), origins);
        setBinding(bindings.name.text, origins);
        node.wholeLocals.add(bindings.name.text);
      } else if (bindings) {
        for (const el of bindings.elements) bind(el.name, (el.propertyName ?? el.name).text);
      }
      lookUp(statement.moduleSpecifier, declaredNames(statement));
    } else if (ts.isExportDeclaration(statement)) {
      if (statement.moduleSpecifier) lookUp(statement.moduleSpecifier, []);
    } else if (ts.isImportEqualsDeclaration(statement)) {
      if (ts.isExternalModuleReference(statement.moduleReference)) {
        const origins: Origin[] = [];
        originsOfModule(checker, checker.getSymbolAtLocation(statement.moduleReference.expression), origins);
        setBinding(statement.name.text, origins);
        node.wholeLocals.add(statement.name.text);
        lookUp(statement.moduleReference.expression, [statement.name.text]);
      } else {
        bind(statement.name, undefined);
      }
    }
  }
  const js = JAVASCRIPT.test(sourceFile.fileName);
  if (WHOLE_MODULE_HINT.test(sourceFile.text) || (js && JSDOC_IMPORT_HINT.test(sourceFile.text))) {
    let holder: ts.Node = sourceFile.statements[0] ?? sourceFile.endOfFileToken;
    /** The statement holding a binding reads it, as it would read an import. */
    const holderReads = (local: string): void => {
      const names = holder.kind === ts.SyntaxKind.EndOfFileToken ? [] : declaredNames(holder as ts.Statement);
      for (const name of [...names, ...jsDocNames(holder, js)]) node.shape.refs.get(name)?.add(local);
      if (names.length === 0) node.shape.loose.find((loose) => loose.pos === holder.pos)?.refs.add(local);
    };
    const visit = (n: ts.Node): void => {
      // `@import { T } from 'm'`: an import declaration written in JSDoc.
      if (ts.isJSDocImportTag(n)) {
        const clause = n.importClause;
        const bindings = clause?.namedBindings;
        if (clause?.name) bind(clause.name, 'default');
        if (bindings && ts.isNamespaceImport(bindings)) {
          const origins: Origin[] = [];
          originsOfModule(checker, checker.getSymbolAtLocation(n.moduleSpecifier), origins);
          setBinding(bindings.name.text, origins);
          node.wholeLocals.add(bindings.name.text);
        } else if (bindings) {
          for (const el of bindings.elements) bind(el.name, (el.propertyName ?? el.name).text);
        }
        lookUp(n.moduleSpecifier, [
          ...(clause?.name ? [clause.name.text] : []),
          ...(bindings && ts.isNamespaceImport(bindings) ? [bindings.name.text] : []),
          ...(bindings && ts.isNamedImports(bindings) ? bindings.elements.map((el) => el.name.text) : []),
        ]);
        return;
      }
      let specifier: ts.Node | undefined;
      /** `import('m').T` reads `T` only; `typeof import('m')` reads the whole module. */
      let named: ts.Identifier | undefined;
      if (ts.isImportTypeNode(n) && ts.isLiteralTypeNode(n.argument)) {
        specifier = n.argument.literal;
        let qualifier = n.qualifier;
        while (qualifier && ts.isQualifiedName(qualifier)) qualifier = qualifier.left;
        named = qualifier;
      } else if (
        ts.isCallExpression(n) &&
        (n.expression.kind === ts.SyntaxKind.ImportKeyword ||
          (ts.isIdentifier(n.expression) && n.expression.text === 'require')) &&
        n.arguments[0] &&
        ts.isStringLiteralLike(n.arguments[0])
      ) {
        specifier = n.arguments[0];
      }
      if (specifier) {
        const local = `${WHOLE_MODULE}${node.bindings.size}`;
        const origins: Origin[] = [];
        const symbol = named && checker.getSymbolAtLocation(named);
        if (symbol) originsOfSymbol(checker, symbol, origins);
        else originsOfModule(checker, checker.getSymbolAtLocation(specifier), origins);
        setBinding(local, origins);
        if (!symbol) node.wholeLocals.add(local);
        lookUp(specifier as ts.Expression, [local]);
        holderReads(local);
      }
      forEachChildAndJSDoc(n, js, visit);
    };
    for (const statement of statementsOf(sourceFile, js)) {
      holder = statement;
      visit(statement);
    }
  }
  return node;
}

function targetsOf(node: FileNode): Set<string> {
  const targets = new Set<string>();
  for (const origins of node.bindings.values()) for (const origin of origins) targets.add(origin.file);
  return targets;
}

function addNode(graph: ImportGraph, file: string, node: FileNode): void {
  graph.nodes.set(file, node);
  for (const target of targetsOf(node)) {
    const set = graph.importers.get(target) ?? new Set<string>();
    set.add(file);
    graph.importers.set(target, set);
  }
}

/**
 * The import graph of `program`'s own (not installed) files, keyed by path
 * relative to `root`.
 */
export function importGraphOf(program: ts.Program, root: string): ImportGraph {
  const checker = program.getTypeChecker();
  const graph: ImportGraph = { nodes: new Map(), importers: new Map() };
  for (const sourceFile of program.getSourceFiles()) {
    if (program.isSourceFileDefaultLibrary(sourceFile) || isInstalled(sourceFile.fileName)) continue;
    addNode(graph, relativeTo(root, sourceFile.fileName), nodeOf(checker, root, sourceFile));
  }
  return graph;
}

/**
 * One graph from the graphs of several programs. A file two programs hold
 * keeps every binding either read, and is a script when either reads it as
 * one: reaching more is the safe side.
 */
export function mergeGraphs(graphs: ImportGraph[]): ImportGraph {
  const merged: ImportGraph = { nodes: new Map(), importers: new Map() };
  for (const graph of graphs) {
    for (const [file, node] of graph.nodes) {
      const known = merged.nodes.get(file);
      if (!known) {
        addNode(merged, file, node);
        continue;
      }
      known.shape.global ||= node.shape.global;
      for (const [name, refs] of node.shape.refs) {
        const into = known.shape.refs.get(name) ?? new Set<string>();
        for (const ref of refs) into.add(ref);
        known.shape.refs.set(name, into);
        if (!known.shape.declared.has(name)) known.shape.declared.set(name, node.shape.declared.get(name) ?? []);
      }
      for (const [local, origins] of node.bindings) {
        const into = known.bindings.get(local) ?? [];
        for (const origin of origins) {
          if (!into.some((o) => o.file === origin.file && o.name === origin.name)) into.push(origin);
        }
        known.bindings.set(local, into);
      }
      for (const [local, imported] of node.importedNames) known.importedNames.set(local, imported);
      for (const local of node.wholeLocals) known.wholeLocals.add(local);
      for (const entry of node.stems) {
        if (!known.stems.some((e) => JSON.stringify(e) === JSON.stringify(entry))) known.stems.push(entry);
      }
    }
  }
  // Importers follow the merged bindings.
  merged.importers.clear();
  for (const [file, node] of merged.nodes) {
    for (const target of targetsOf(node)) {
      const set = merged.importers.get(target) ?? new Set<string>();
      set.add(file);
      merged.importers.set(target, set);
    }
  }
  return merged;
}

/** What an edit reaches: the files, whether an export was added or removed, and the files holding an augmentation. */
export type Reached = { files: Set<string>; exportsMoved: boolean; augmented: Set<string> } | 'all';

/**
 * The files `changes` can reach in the program `graph` was read from, or
 * `'all'` when a change reaches every file. Read the graph from the program
 * as it was before the change. A file that is added or deleted has `''` on its
 * missing side.
 */
export function affectedFiles(graph: ImportGraph, changes: FileChange[]): Reached {
  /** Changed top-level names, by file; `*` is every name of the file. */
  const changed = new Map<string, Set<string>>();
  const affected = new Set<string>();
  const addedOrRemoved = new Set<string>();
  /** The changed files whose exports were added or removed, and whether one had none before. */
  const exportsChanged = new Set<string>();
  let exportedNothing = false;
  const queue: string[] = [];
  /** The files with a changed script, global declaration or module augmentation, or one that reads a changed name. */
  const augmented = new Set<string>();
  /** The stems of the added and deleted files. */
  const moved = new Set<string>();
  const grow = (file: string, names: Iterable<string>): void => {
    const known = changed.get(file) ?? new Set<string>();
    const size = known.size;
    for (const name of names) {
      known.add(name);
      if (isAugmentation(name)) augmented.add(file);
    }
    changed.set(file, known);
    if (known.size !== size) queue.push(file);
  };
  for (const change of changes) {
    if (change.before === change.after) continue;
    if (isInstalled(change.file) || !SOURCE_EXTENSION.test(change.file)) return 'all';
    const added = change.before === '';
    const deleted = change.after === '';
    // A missing side is an empty module, never a script.
    const before = shapeOf(change.file, added ? EMPTY_MODULE : change.before);
    const after = shapeOf(change.file, deleted ? EMPTY_MODULE : change.after);
    // The program reads the file by its own rules; when it and the text
    // disagree, the file is a script.
    const script = before.global || after.global || (graph.nodes.get(change.file)?.shape.global ?? false);
    if (!deleted) affected.add(change.file);
    if (script) {
      augmented.add(change.file);
      continue;
    }
    if (added || deleted) {
      // Nothing imported a name from a file that did not exist, or no longer
      // does: what the change reaches is the file's importers, and imports
      // that looked up its stem.
      moved.add(stemOf(change.file));
      grow(change.file, [...changedNames(change.file, before, after), '*']);
      continue;
    }
    const exported = [
      ...[...before.exports].filter((name) => !after.exports.has(name)),
      ...[...after.exports].filter((name) => !before.exports.has(name)),
      ...[...after.exportTargets].filter(([name, local]) => before.exportTargets.has(name) && before.exportTargets.get(name) !== local).map(([name]) => name),
    ];
    for (const name of exported) addedOrRemoved.add(name);
    if (exported.length > 0) exportsChanged.add(change.file);
    if (exported.length > 0 && before.exports.size === 0) exportedNothing = true;
    grow(change.file, changedNames(change.file, before, after));
  }
  if (moved.size > 0) {
    // An import looked up at a stem the change adds or removes may resolve
    // elsewhere now: its file is reached, and so is whoever read the file it
    // resolved to.
    for (const [file, node] of graph.nodes) {
      for (const entry of node.stems) {
        if (!entry.stems.some((stem) => moved.has(stem))) continue;
        affected.add(file);
        grow(file, closeOver(node.shape, entry.locals));
        if (entry.target !== undefined) grow(entry.target, ['*']);
      }
    }
  }
  if (addedOrRemoved.size > 0) {
    // Which declaration an import of that name resolves to may have moved.
    for (const [file, node] of graph.nodes) {
      const hit = [...node.importedNames].filter(([, imported]) => addedOrRemoved.has(imported)).map(([local]) => local);
      // A whole module reads the new or removed name when it re-exports the
      // changed file, which shows as an origin there unless that file
      // exported nothing before.
      for (const local of node.wholeLocals) {
        if (exportedNothing || node.bindings.get(local)!.some((origin) => exportsChanged.has(origin.file))) hit.push(local);
      }
      if (hit.length === 0) continue;
      affected.add(file);
      grow(file, closeOver(node.shape, hit));
    }
  }
  const reaches = (origin: Origin): boolean => {
    const names = changed.get(origin.file);
    if (!names || names.size === 0) return false;
    return origin.name === '*' || names.has('*') || names.has(origin.name);
  };
  while (queue.length > 0) {
    const target = queue.shift()!;
    for (const importer of graph.importers.get(target) ?? []) {
      const node = graph.nodes.get(importer);
      if (!node) continue;
      const hit: string[] = [];
      for (const [local, origins] of node.bindings) if (origins.some(reaches)) hit.push(local);
      if (hit.length === 0) continue;
      affected.add(importer);
      grow(importer, closeOver(node.shape, hit));
    }
  }
  return { files: affected, exportsMoved: addedOrRemoved.size > 0, augmented };
}

// ===========================================================================
// The graph as data
// ===========================================================================

export function serialiseGraph(graph: ImportGraph): SerialGraph {
  const strings: string[] = [];
  const index = new Map<string, number>();
  const id = (text: string): number => {
    let at = index.get(text);
    if (at === undefined) {
      at = strings.length;
      strings.push(text);
      index.set(text, at);
    }
    return at;
  };
  const files: SerialGraph['files'] = [];
  for (const [file, node] of [...graph.nodes].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))) {
    const { shape } = node;
    // An identifier matters to the walk only when it is a name that can be
    // changed: a binding, a whole-module local, or a name the file declares.
    const matters = (ref: string): boolean => node.bindings.has(ref) || shape.declared.has(ref);
    const refs: number[][] = [];
    for (const [name, mentions] of shape.refs) {
      const kept = [...mentions].filter(matters);
      if (kept.length > 0) refs.push([id(name), ...kept.map(id)]);
    }
    const loose: number[][] = [];
    for (const statement of shape.loose) {
      const kept = [...statement.refs].filter(matters);
      if (kept.length > 0) loose.push(kept.map(id));
    }
    files.push({
      p: id(file),
      g: shape.global ? 1 : 0,
      d: [...shape.declared.keys()].map(id),
      r: refs,
      l: loose,
      e: [...shape.exports].map(id),
      t: [...shape.exportTargets].map(([name, local]) => [id(name), id(local)]),
      b: [...node.bindings].map(([local, origins]) => [
        id(local),
        node.importedNames.has(local) ? id(node.importedNames.get(local)!) : -1,
        origins.flatMap((origin) => [id(origin.file), id(origin.name)]),
      ]),
      w: [...node.wholeLocals].map(id),
      s: node.stems.map((entry) => [
        entry.target === undefined ? -1 : id(entry.target),
        entry.locals.map(id),
        entry.stems.map(id),
      ]),
    });
  }
  return { version: 1, strings, files };
}

/** The graph `serialiseGraph` wrote. Throws on an index outside the string table. */
export function graphFromSerial(serial: SerialGraph): ImportGraph {
  const text = (at: number): string => {
    const value = serial.strings[at];
    if (value === undefined) throw new Error(`Graph string ${at} is outside the table`);
    return value;
  };
  const graph: ImportGraph = { nodes: new Map(), importers: new Map() };
  for (const file of serial.files) {
    const shape: Shape = {
      declared: new Map(file.d.map((at) => [text(at), []])),
      refs: new Map(file.r.map(([name, ...mentions]) => [text(name!), new Set(mentions.map(text))])),
      loose: file.l.map((mentions, pos) => ({ pos, text: '', refs: new Set(mentions.map(text)) })),
      exports: new Set(file.e.map(text)),
      exportTargets: new Map(file.t.map(([name, local]) => [text(name!), text(local!)])),
      global: file.g === 1,
    };
    const node: FileNode = {
      shape,
      bindings: new Map(),
      importedNames: new Map(),
      wholeLocals: new Set(file.w.map(text)),
      stems: file.s.map(([target, locals, stems]) => ({
        stems: stems.map(text),
        ...(target === -1 ? {} : { target: text(target) }),
        locals: locals.map(text),
      })),
    };
    for (const [local, imported, origins] of file.b) {
      node.bindings.set(
        text(local),
        Array.from({ length: origins.length / 2 }, (_, i) => ({
          file: text(origins[2 * i]!),
          name: text(origins[2 * i + 1]!),
        }))
      );
      if (imported !== -1) node.importedNames.set(text(local), text(imported));
    }
    addNode(graph, text(file.p), node);
  }
  return graph;
}

/** What `reach` answers. */
export interface ReachResult {
  reached: string[] | 'all';
  augmented: string[];
  exports_moved: boolean;
}

/** `affectedFiles` over a serialised graph, with the answer sorted. */
export function reach(serial: SerialGraph, changes: FileChange[]): ReachResult {
  const answer = affectedFiles(graphFromSerial(serial), changes);
  if (answer === 'all') return { reached: 'all', augmented: [], exports_moved: false };
  return {
    reached: [...answer.files].sort(),
    augmented: [...answer.augmented].sort(),
    exports_moved: answer.exportsMoved,
  };
}
