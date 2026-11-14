/**
 * carrick#2148: where a named type is declared, as a reader would open it.
 * The compiler engine's answers (`type_home`, `stated_body.root_home`) and
 * the bundle's manifest (`home`) read it from here.
 */

import { Node, ts } from 'ts-morph';
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
