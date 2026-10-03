/**
 * carrick#1836: what each bare name in an inference's printed text meant.
 *
 * The structural printer (`type-structural-expander.ts`) hands some subtrees
 * back to the compiler's own print with no enclosing declaration, and that
 * print writes every named type by its bare name: a database client's row,
 * which is a library's mapped type, prints `{ status: EntityStatus; ... }`.
 * The file a request names often never imports `EntityStatus`, so the text
 * names nothing where it is read (TS2304), and the member reads `any` in the
 * capture's surface.
 *
 * The compiler knew the symbol when it printed the name, and only then. So
 * every type print made while an inference runs is noted (`notePrintedType`),
 * and when it ends (`PrintedTypes.namesIn`) the names its text prints that the
 * request's file cannot resolve are looked up in those prints: the node
 * builder gives each name it writes the symbol it wrote it for. Each such
 * symbol is recorded as the module that declares it and its export path
 * there. A name printed for two declarations is recorded twice, and the
 * reader of the record decides what an ambiguous name means.
 *
 * Only the prints whose text writes such a name are read back, and only when
 * the inference's text has one, so an inference whose names all resolve costs
 * no more than keeping the list.
 */

import { ts, type Node, type Type } from 'ts-morph';
import type { PrintedName } from './types.js';

/** One type print: the type, the declaration it was printed for, the text. */
type Print = { type: ts.Type; enclosing: ts.Node | undefined; text: string };

/** The prints of the inference running now, when one is recording. */
let recording: Print[] | undefined;

/**
 * Note a type the compiler printed as `text`, so the inference running now can
 * tell what the names in that print meant. Does nothing outside a recording.
 */
export function notePrintedType(type: Type, enclosing: Node | undefined, text: string): void {
  recording?.push({ type: type.compilerType, enclosing: enclosing?.compilerNode, text });
}

/**
 * Same flags the printers pass `typeToString` (`NoTruncation`, `InTypeAlias`),
 * so the node read back is the one that was printed.
 */
const NODE_FLAGS =
  ts.NodeBuilderFlags.NoTruncation | ts.NodeBuilderFlags.InTypeAlias | ts.NodeBuilderFlags.IgnoreErrors;

const REFERENCE_MEANING = ts.SymbolFlags.Type | ts.SymbolFlags.Namespace | ts.SymbolFlags.Alias;

/** The type prints one inference made. */
export class PrintedTypes {
  private readonly prints: Print[] = [];

  /** Run `read`, noting every type it prints. */
  during<T>(read: () => T): T {
    const outer = recording;
    recording = this.prints;
    try {
      return read();
    } finally {
      recording = outer;
    }
  }

  /**
   * The declarations behind each name `texts` print that `source` does not
   * resolve, sorted, one entry per distinct declaration. `checker` must be
   * the one the prints were made with: call this before the program changes.
   */
  namesIn(texts: readonly string[], source: ts.SourceFile, checker: ts.TypeChecker): PrintedName[] {
    const wanted = new Set<string>();
    for (const text of texts) {
      for (const name of referencedNames(text)) {
        if (!checker.resolveName(name, source, REFERENCE_MEANING, false)) wanted.add(name);
      }
    }
    if (wanted.size === 0) return [];
    // Only a print whose text writes one of the names can say what it meant.
    const alternatives = [...wanted].map((name) => name.replace(/\$/g, '\\$')).join('|');
    const writes = new RegExp(`(?<![\\w$.])(?:${alternatives})(?![\\w$])`);

    const found = new Map<string, PrintedName>();
    const identities = new Map<ts.Symbol, Identity | undefined>();
    const visited = new Map<ts.Type, Set<ts.Node | undefined>>();
    for (const { type, enclosing, text } of this.prints) {
      if (!writes.test(text)) continue;
      const at = visited.get(type) ?? new Set<ts.Node | undefined>();
      if (at.has(enclosing)) continue;
      visited.set(type, at.add(enclosing));
      let node: ts.TypeNode | undefined;
      try {
        node = checker.typeToTypeNode(type, enclosing, NODE_FLAGS);
      } catch {
        continue;
      }
      const visit = (current: ts.Node): void => {
        if (ts.isTypeReferenceNode(current)) {
          const id = leftmost(current.typeName);
          const name = ts.idText(id);
          const symbol = wanted.has(name) ? symbolOf(id, checker) : undefined;
          if (symbol) {
            if (!identities.has(symbol)) identities.set(symbol, identityOf(symbol, checker));
            const identity = identities.get(symbol);
            if (identity) {
              found.set(`${name}\0${identity.file}\0${identity.export_path.join('.')}`, { name, ...identity });
            }
          }
        }
        ts.forEachChild(current, visit);
      };
      if (node) visit(node);
    }
    return [...found.entries()].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)).map(([, entry]) => entry);
  }
}

type Identity = Pick<PrintedName, 'file' | 'export_path'>;

function leftmost(name: ts.EntityName): ts.Identifier {
  return ts.isIdentifier(name) ? name : leftmost(name.left);
}

/**
 * The leftmost names of the type references `text` makes, less the type
 * parameters the text declares itself (`[K in ...]`, `infer U`).
 */
function referencedNames(text: string): Set<string> {
  const parsed = ts.createSourceFile('printed.ts', `type __Printed = ${text};`, ts.ScriptTarget.Latest, true);
  const names = new Set<string>();
  const declared = new Set<string>();
  const visit = (node: ts.Node): void => {
    if (ts.isTypeParameterDeclaration(node)) declared.add(node.name.text);
    if (ts.isTypeReferenceNode(node)) names.add(ts.idText(leftmost(node.typeName)));
    ts.forEachChild(node, visit);
  };
  visit(parsed);
  for (const name of declared) names.delete(name);
  names.delete('__Printed');
  return names;
}

/**
 * The symbol a printed name was written for. The node builder sets it on
 * every identifier it creates (`symbol` is not in the public typings); a name
 * it copied from source carries none, and is read at its original node.
 */
function symbolOf(id: ts.Identifier, checker: ts.TypeChecker): ts.Symbol | undefined {
  const written = (id as ts.Identifier & { symbol?: ts.Symbol }).symbol;
  if (written) return written;
  const original = ts.getOriginalNode(id);
  return original !== id && original.parent ? checker.getSymbolAtLocation(original) : undefined;
}

/**
 * The module that declares `symbol`'s type and the export path to it there,
 * or undefined for a type no module export reaches: a global, a type
 * parameter, a declaration a function body holds, a module itself.
 */
function identityOf(symbol: ts.Symbol, checker: ts.TypeChecker): Identity | undefined {
  const resolve = (s: ts.Symbol): ts.Symbol =>
    s.flags & ts.SymbolFlags.Alias ? checker.getAliasedSymbol(s) : s;
  const target = resolve(symbol);
  if (
    !(target.flags & (ts.SymbolFlags.Type | ts.SymbolFlags.Namespace)) ||
    target.flags & ts.SymbolFlags.TypeParameter
  ) {
    return undefined;
  }
  const declaration = target.declarations?.[0];
  if (!declaration || ts.isSourceFile(declaration)) return undefined;
  const file = declaration.getSourceFile();
  const moduleSymbol = checker.getSymbolAtLocation(file);
  if (!moduleSymbol) return undefined;

  const direct = checker.getExportsOfModule(moduleSymbol).filter((e) => resolve(e) === target);
  if (direct.length > 0) {
    const chosen = direct.find((e) => e.getName() === target.getName()) ?? direct[0];
    return { file: file.fileName, export_path: [chosen.getName()] };
  }
  // A namespace member: `"<module>".Billing.Kind`, checked step by step.
  const qualified = checker.getFullyQualifiedName(target);
  const close = qualified.startsWith('"') ? qualified.indexOf('"', 1) : -1;
  if (close < 0) return undefined;
  const exportPath = qualified.slice(close + 2).split('.');
  let current: ts.Symbol | undefined = moduleSymbol;
  for (const part of exportPath) {
    const next: ts.Symbol | undefined = current
      ? checker.getExportsOfModule(current).find((e) => e.getName() === part)
      : undefined;
    current = next && resolve(next);
  }
  return current === target ? { file: file.fileName, export_path: exportPath } : undefined;
}
