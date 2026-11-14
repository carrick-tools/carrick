/**
 * carrick#2148: where a named type is declared, as a reader would open it.
 * The compiler engine's answers (`type_home`, `stated_body.root_home`) and
 * the bundle's manifest (`home`) read it from here.
 */

import { Node, ts, VariableDeclarationKind } from 'ts-morph';
import { startLineOf } from './line-index.js';
import type { TypeHome } from './types.js';

/** A declaration that names a type: an interface, a type alias, a class or an enum. */
export function isTypeSpaceDeclaration(declaration: Node): boolean {
  return (
    Node.isInterfaceDeclaration(declaration) ||
    Node.isTypeAliasDeclaration(declaration) ||
    Node.isClassDeclaration(declaration) ||
    Node.isEnumDeclaration(declaration)
  );
}

/**
 * The name a reader knows a type declaration by. The compiler names a default
 * export `default`; the name it declares (`export default interface Ticket`)
 * is the one a reader can import. `undefined` for a default export with no
 * name (`export default class {}`).
 */
export function declaredTypeName(name: string, declaration: Node): string | undefined {
  if (name !== 'default') return name;
  return (declaration as { getNameNode?: () => Node | undefined }).getNameNode?.()?.getText();
}

/**
 * The home a type declaration gives under the name `name`: the declaring
 * file (absolute) and the 1-based line of the declaration's name, never of a
 * JSDoc block above it. A default export (`export default interface
 * Ticket`, the symbol `default`) is named by the name it declares.
 *
 * None where the declaration is not the repository's or a package's own
 * contract:
 *  - a TypeScript lib file (`Uint8Array`, `Blob`);
 *  - a declaration inside an ambient `declare module "<specifier>"` block,
 *    which states a package's types wherever the stub file sits;
 *  - a declaration with no name to point at (`export default class {}`).
 */
export function homeOfTypeDeclaration(
  name: string,
  declaration: Node,
  program: ts.Program
): TypeHome | undefined {
  const sourceFile = declaration.getSourceFile();
  if (sourceFile.compilerNode.hasNoDefaultLib || program.isSourceFileDefaultLibrary(sourceFile.compilerNode)) {
    return undefined;
  }
  const inAmbientModule = declaration
    .getAncestors()
    .some((ancestor) => Node.isModuleDeclaration(ancestor) && Node.isStringLiteral(ancestor.getNameNode()));
  if (inAmbientModule) return undefined;
  const nameNode = (declaration as { getNameNode?: () => Node | undefined }).getNameNode?.();
  const symbol = declaredTypeName(name, declaration);
  if (!nameNode || symbol === undefined) return undefined;
  return { symbol, file_path: sourceFile.getFilePath(), line: startLineOf(nameNode) };
}

/**
 * carrick#2148 H3: the home of a schema value a contract was read off, where
 * the value is one name for a `const` the repository declares
 * (`guard('json', NewTicket)`, `NewTicket.safeParse(body)`). The schema is
 * where that contract is written, so the home is the const's name, marked
 * `kind: "schema"`: it is a value, never a type a reader can import as one.
 *
 * None for a schema built at its use (`record({ … })`), a name that is not a
 * `const` (it can be reassigned, so the declaration is not the contract), an
 * ambient `declare const`, and a declaration in a package or a `.d.ts`.
 */
export function homeOfSchemaValue(value: Node): TypeHome | undefined {
  if (!Node.isIdentifier(value)) return undefined;
  let symbol = value.getSymbol();
  if (symbol?.isAlias()) symbol = symbol.getAliasedSymbol() ?? symbol;
  const declarations = symbol?.getDeclarations() ?? [];
  if (declarations.length !== 1) return undefined;
  const [declaration] = declarations;
  if (!Node.isVariableDeclaration(declaration) || !declaration.hasInitializer()) return undefined;
  const list = declaration.getParent();
  if (!Node.isVariableDeclarationList(list) || list.getDeclarationKind() !== VariableDeclarationKind.Const) {
    return undefined;
  }
  const sourceFile = declaration.getSourceFile();
  if (sourceFile.isDeclarationFile() || sourceFile.isInNodeModules()) return undefined;
  const nameNode = declaration.getNameNode();
  if (!Node.isIdentifier(nameNode)) return undefined;
  return {
    symbol: nameNode.getText(),
    file_path: sourceFile.getFilePath(),
    line: startLineOf(nameNode),
    kind: 'schema',
  };
}
