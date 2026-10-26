/**
 * Which files an edit can reach (carrick#2881).
 *
 * A daemon holding a service answers `infer` items again only where an edit
 * can change the answer. This module says where: the changed files, and every
 * file whose types read a declaration the edit changed, followed name by name
 * rather than file by file, so a barrel (`index.ts` with `export *`) does not
 * turn one changed export into every importer of the package.
 *
 * The walk:
 * - In a changed file, a top-level name is changed when the text of a
 *   statement declaring it changed, or when a statement declaring it mentions
 *   a changed name of the same file (so `type Dto = z.infer<typeof schema>`
 *   changes with `schema`). A statement that declares nothing (an expression
 *   statement, which can add an expando member to a function) changes every
 *   name it mentions when it changes or mentions a changed name.
 * - An importing file is reached when one of its imports resolves, through
 *   every re-export (the compiler's own alias resolution), to a changed
 *   declaration. A namespace import reads every export of its module, and so
 *   does a dynamic `import()`, `require()` or `import('m').T`, read as a
 *   binding of the statement that holds it. Its names that
 *   mention the reached imports change in turn, and the walk goes on to their
 *   importers. A name whose declaration mentions no changed name stops the walk
 *   there, whatever its printed type says.
 * - A global declaration or a module augmentation (`declare global`, `declare
 *   module 'x'`) is read by files that never import it (a value returned by
 *   the augmented package carries it), so a change to one, or to a name one
 *   mentions, reaches every file. So does any change to a script (a file with
 *   no import or export, all of it global), a file that is not TypeScript or
 *   JavaScript source, or an installed package's file. An import that resolves
 *   to an augmented interface reads the augmentation by its own name
 *   (`AUGMENTATION`), so an unchanged augmentation stops nothing.
 * - An export added or removed (an export list, or a new exported declaration)
 *   reaches every file importing that name from anywhere, and the statements
 *   of every file holding a namespace or dynamic import that read it.
 *
 * Dependencies run from a file to the declarations it names; the compiler
 * never types a file from the files that import it, so nothing is followed the
 * other way. One assumption: an installed package's declarations do not import
 * the workspace's own sources (only a workspace package could, and its files
 * are read at their real path, outside `node_modules`).
 *
 * The graph is read from a program's own checker, once per program. Read it
 * from the program that answered the last request: building it creates no
 * type, but it does resolve every import.
 */

import { ts } from 'ts-morph';

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

function isInstalled(file: string): boolean {
  return file.includes('/node_modules/');
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

function mentioned(node: ts.Node): Set<string> {
  const refs = new Set<string>();
  const visit = (n: ts.Node): void => {
    if (ts.isIdentifier(n)) refs.add(n.text);
    n.forEachChild(visit);
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

/** A file's shape from its text. */
export function shapeOf(file: string, text: string): Shape {
  const sourceFile = ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true, scriptKindOf(file));
  return shapeOfSource(sourceFile);
}

function shapeOfSource(sourceFile: ts.SourceFile): Shape {
  const shape: Shape = {
    declared: new Map(),
    refs: new Map(),
    loose: [],
    exports: new Set(),
    exportTargets: new Map(),
    global: !ts.isExternalModule(sourceFile),
  };
  const declare = (name: string, text: string, refs: Set<string>): void => {
    shape.declared.set(name, [...(shape.declared.get(name) ?? []), text]);
    const known = shape.refs.get(name) ?? new Set<string>();
    for (const ref of refs) known.add(ref);
    shape.refs.set(name, known);
  };
  for (const statement of sourceFile.statements) {
    // With its leading comments: a JSDoc comment types a JavaScript file.
    const text = statement.getFullText(sourceFile);
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
      shape.loose.push({ pos: statement.pos, text, refs: mentioned(statement) });
      continue;
    }
    const refs = ts.isImportDeclaration(statement) || ts.isImportEqualsDeclaration(statement)
      ? new Set<string>()
      : mentioned(statement);
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
function changedNames(before: Shape, after: Shape): Set<string> {
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

/** The name a dynamic import, `require()` or import type is bound under: no identifier can spell it. */
const WHOLE_MODULE = '\u0000reads module ';

function nodeOf(checker: ts.TypeChecker, sourceFile: ts.SourceFile): FileNode {
  const node: FileNode = {
    shape: shapeOfSource(sourceFile),
    bindings: new Map(),
    importedNames: new Map(),
    wholeLocals: new Set(),
  };
  const bind = (local: ts.Identifier, imported: string | undefined): void => {
    const origins: Origin[] = [];
    originsOfSymbol(checker, checker.getSymbolAtLocation(local), origins);
    node.bindings.set(local.text, origins);
    if (imported !== undefined) node.importedNames.set(local.text, imported);
  };
  for (const statement of sourceFile.statements) {
    if (ts.isImportDeclaration(statement)) {
      const clause = statement.importClause;
      if (!clause) continue;
      if (clause.name) bind(clause.name, 'default');
      const bindings = clause.namedBindings;
      if (bindings && ts.isNamespaceImport(bindings)) {
        const origins: Origin[] = [];
        originsOfModule(checker, checker.getSymbolAtLocation(statement.moduleSpecifier), origins);
        node.bindings.set(bindings.name.text, origins);
        node.wholeLocals.add(bindings.name.text);
      } else if (bindings) {
        for (const el of bindings.elements) bind(el.name, (el.propertyName ?? el.name).text);
      }
    } else if (ts.isImportEqualsDeclaration(statement)) {
      if (ts.isExternalModuleReference(statement.moduleReference)) {
        const origins: Origin[] = [];
        originsOfModule(checker, checker.getSymbolAtLocation(statement.moduleReference.expression), origins);
        node.bindings.set(statement.name.text, origins);
        node.wholeLocals.add(statement.name.text);
      } else {
        bind(statement.name, undefined);
      }
    }
  }
  if (WHOLE_MODULE_HINT.test(sourceFile.text)) {
    let holder: ts.Statement = sourceFile.statements[0]!;
    const visit = (n: ts.Node): void => {
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
        node.bindings.set(local, origins);
        if (!symbol) node.wholeLocals.add(local);
        // The statement holding it reads it, as it would read an import.
        const names = declaredNames(holder);
        for (const name of names) node.shape.refs.get(name)?.add(local);
        if (names.length === 0) node.shape.loose.find((loose) => loose.pos === holder.pos)?.refs.add(local);
      }
      n.forEachChild(visit);
    };
    for (const statement of sourceFile.statements) {
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

/** The import graph of `program`'s own (not installed) files. */
export function importGraphOf(program: ts.Program): ImportGraph {
  const checker = program.getTypeChecker();
  const graph: ImportGraph = { nodes: new Map(), importers: new Map() };
  for (const sourceFile of program.getSourceFiles()) {
    if (program.isSourceFileDefaultLibrary(sourceFile) || isInstalled(sourceFile.fileName)) continue;
    addNode(graph, sourceFile.fileName, nodeOf(checker, sourceFile));
  }
  return graph;
}

/**
 * Read `files` again into `graph` from `program`, which holds them as they
 * are now. Every other file's node stands: an edit the graph is carried
 * across changes no import's module and moves no export (the freshness sweep
 * rebuilds on the first, `affectedFiles` reports the second).
 */
export function readAgain(graph: ImportGraph, program: ts.Program, files: Iterable<string>): void {
  const checker = program.getTypeChecker();
  for (const file of files) {
    const old = graph.nodes.get(file);
    if (old) for (const target of targetsOf(old)) graph.importers.get(target)?.delete(file);
    graph.nodes.delete(file);
    const sourceFile = program.getSourceFile(file);
    if (sourceFile) addNode(graph, file, nodeOf(checker, sourceFile));
  }
}

/** What an edit reaches: the files, and whether an export was added or removed. */
export type Reached = { files: Set<string>; exportsMoved: boolean } | 'all';

/**
 * The files `changes` can reach in the program `graph` was read from, or
 * `'all'` when a change reaches every file. Read the graph from the program
 * as it was before the change.
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
  /** A changed name of an augmentation or a global declaration: every file is reached. */
  let everything = false;
  const grow = (file: string, names: Iterable<string>): void => {
    const known = changed.get(file) ?? new Set<string>();
    const size = known.size;
    for (const name of names) {
      known.add(name);
      if (isAugmentation(name)) everything = true;
    }
    changed.set(file, known);
    if (known.size !== size) queue.push(file);
  };
  for (const change of changes) {
    if (change.before === change.after) continue;
    if (isInstalled(change.file) || !SOURCE_EXTENSION.test(change.file) || change.file.endsWith('.json')) return 'all';
    const before = shapeOf(change.file, change.before);
    const after = shapeOf(change.file, change.after);
    if (before.global || after.global) return 'all';
    affected.add(change.file);
    const moved = [
      ...[...before.exports].filter((name) => !after.exports.has(name)),
      ...[...after.exports].filter((name) => !before.exports.has(name)),
      ...[...after.exportTargets].filter(([name, local]) => before.exportTargets.has(name) && before.exportTargets.get(name) !== local).map(([name]) => name),
    ];
    for (const name of moved) addedOrRemoved.add(name);
    if (moved.length > 0) exportsChanged.add(change.file);
    if (moved.length > 0 && before.exports.size === 0) exportedNothing = true;
    grow(change.file, changedNames(before, after));
    if (everything) return 'all';
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
      if (everything) return 'all';
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
      if (everything) return 'all';
    }
  }
  return { files: affected, exportsMoved: addedOrRemoved.size > 0 };
}

/**
 * Every file `file`'s types can read in the program `graph` was read from:
 * the file, and the files of every declaration its imports resolve to, and
 * theirs in turn.
 */
export function readsOf(graph: ImportGraph, file: string): Set<string> {
  const seen = new Set<string>([file]);
  const queue = [file];
  while (queue.length > 0) {
    const node = graph.nodes.get(queue.shift()!);
    if (!node) continue;
    const next = (origin: Origin): void => {
      if (seen.has(origin.file)) return;
      seen.add(origin.file);
      queue.push(origin.file);
    };
    for (const origins of node.bindings.values()) origins.forEach(next);
  }
  return seen;
}
