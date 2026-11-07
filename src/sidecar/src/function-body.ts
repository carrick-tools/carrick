/**
 * The function body a call runs (carrick#3500). One answer for the helpers
 * that ask it: a declaration that has a body is that body; an overloaded
 * function is its one implementation (the signatures carry no body); a
 * variable or property initialised with an arrow or function expression is
 * that function, through the wrappers that leave a value as it is
 * (parentheses, `as`, `<T>`, `!`, `satisfies`).
 *
 * A name with two bodies, or with none, has no one body and answers
 * undefined.
 */

import {
  Node,
  type ArrowFunction,
  type FunctionDeclaration,
  type FunctionExpression,
  type MethodDeclaration,
} from 'ts-morph';

export type PlainFunctionBody = FunctionDeclaration | ArrowFunction | FunctionExpression;
export type FunctionBody = PlainFunctionBody | MethodDeclaration;

/** An expression without the wrappers that leave its value as it is. */
export function peelValueWrappers(node: Node): Node {
  let current = node;
  while (
    Node.isParenthesizedExpression(current) ||
    Node.isAsExpression(current) ||
    Node.isTypeAssertion(current) ||
    Node.isSatisfiesExpression(current) ||
    Node.isNonNullExpression(current)
  ) {
    current = current.getExpression();
  }
  return current;
}

function bodyOfDeclaration(declaration: Node, methods: boolean): FunctionBody | undefined {
  if (Node.isFunctionDeclaration(declaration)) return declaration.getBody() ? declaration : undefined;
  if (Node.isMethodDeclaration(declaration)) return methods && declaration.getBody() ? declaration : undefined;
  if (Node.isArrowFunction(declaration) || Node.isFunctionExpression(declaration)) return declaration;
  if (Node.isVariableDeclaration(declaration) || Node.isPropertyAssignment(declaration)) {
    const initializer = declaration.getInitializer();
    const value = initializer ? peelValueWrappers(initializer) : undefined;
    if (value && (Node.isArrowFunction(value) || Node.isFunctionExpression(value))) return value;
  }
  return undefined;
}

/**
 * The one function with a body among `declarations`, or undefined for none
 * or several. `methods: false` leaves method declarations out.
 */
export function functionBodyOf(declarations: readonly Node[], options: { methods: false }): PlainFunctionBody | undefined;
export function functionBodyOf(declarations: readonly Node[], options?: { methods?: boolean }): FunctionBody | undefined;
export function functionBodyOf(
  declarations: readonly Node[],
  options: { methods?: boolean } = {}
): FunctionBody | undefined {
  const methods = options.methods ?? true;
  const bodies = new Set<FunctionBody>();
  for (const declaration of declarations) {
    const body = bodyOfDeclaration(declaration, methods);
    if (body) bodies.add(body);
  }
  return bodies.size === 1 ? [...bodies][0] : undefined;
}
