/**
 * Whether a handler still works when no request body arrives (carrick#2814).
 *
 * A server library hands a handler `undefined` where a request carried no
 * body, whatever the handler's type says. A handler typed `{ limit?: number }`
 * that destructures the body, or reads `body.limit` without `?.`, throws on
 * that call, so a route that dereferences its body requires one even when its
 * type has no required member. These readers answer the question from the
 * handler's own source, by shape and with no library name consulted:
 *
 *  - every read of the body tolerates `undefined`: `body ?? {}`, `body || {}`,
 *    `body?.x`, a comparison, `typeof body`, `!body`, a condition, an object
 *    spread, or a binding default (`{ body = {} }`, `(payload = {}) =>`);
 *  - a name the body is bound to is followed to each of its own reads;
 *  - anything else (a destructure with no default, a plain member read, the
 *    body handed to a function) dereferences it, or may, so the answer is no.
 *
 * Conservative by construction: a read these rules do not recognise answers
 * no, and the check keeps the reading it had without the mark.
 */

import { Node, SyntaxKind, type BindingElement, type Identifier, type ParameterDeclaration } from 'ts-morph';
import type { HandlerFunction } from './stated-params.js';

/** How many names a body may be rebound through before the reads are not followed. */
const ALIAS_DEPTH = 3;

/** The member a request object carries its parsed body on. */
const BODY_MEMBER = 'body';

/** Wrappers that change nothing about the value at runtime. */
function isTransparentWrapper(node: Node, child: Node): boolean {
  return (
    (Node.isParenthesizedExpression(node) ||
      Node.isNonNullExpression(node) ||
      Node.isAsExpression(node) ||
      Node.isSatisfiesExpression(node) ||
      Node.isTypeAssertion(node)) &&
    node.getExpression() === child
  );
}

/** The function the declaration of `name` sits in, whose reads of it are counted. */
function scopeOf(name: Node): Node | undefined {
  return (
    name.getFirstAncestor(
      (ancestor) =>
        Node.isFunctionDeclaration(ancestor) ||
        Node.isArrowFunction(ancestor) ||
        Node.isFunctionExpression(ancestor) ||
        Node.isMethodDeclaration(ancestor)
    ) ?? name.getSourceFile()
  );
}

/**
 * Every read of the name `name` declares tolerates `undefined`. A name that is
 * never read tolerates it. A shorthand property (`{ body }`) hands the value
 * on, so it counts as a read that may dereference it.
 */
function nameToleratesAbsence(name: Identifier, depth: number): boolean {
  if (depth > ALIAS_DEPTH) return false;
  const symbol = name.getSymbol();
  const scope = scopeOf(name);
  if (!symbol || !scope) return false;
  for (const shorthand of scope.getDescendantsOfKind(SyntaxKind.ShorthandPropertyAssignment)) {
    if (shorthand.getName() === name.getText()) return false;
  }
  for (const reference of scope.getDescendantsOfKind(SyntaxKind.Identifier)) {
    if (reference === name || reference.getSymbol() !== symbol) continue;
    if (!valueToleratesAbsence(reference, depth)) return false;
  }
  return true;
}

/** The read `value` (an expression that evaluates to the body) tolerates `undefined`. */
function valueToleratesAbsence(value: Node, depth: number): boolean {
  let node = value;
  let parent = node.getParent();
  while (parent && isTransparentWrapper(parent, node)) {
    node = parent;
    parent = node.getParent();
  }
  if (!parent) return false;

  if (Node.isBinaryExpression(parent)) {
    const operator = parent.getOperatorToken().getKind();
    if (
      parent.getLeft() === node &&
      (operator === SyntaxKind.QuestionQuestionToken ||
        operator === SyntaxKind.BarBarToken ||
        operator === SyntaxKind.AmpersandAmpersandToken)
    ) {
      return true;
    }
    return (
      operator === SyntaxKind.EqualsEqualsEqualsToken ||
      operator === SyntaxKind.ExclamationEqualsEqualsToken ||
      operator === SyntaxKind.EqualsEqualsToken ||
      operator === SyntaxKind.ExclamationEqualsToken
    );
  }
  if (
    (Node.isPropertyAccessExpression(parent) || Node.isElementAccessExpression(parent)) &&
    parent.getExpression() === node
  ) {
    return parent.hasQuestionDotToken();
  }
  if (Node.isTypeOfExpression(parent)) return true;
  if (Node.isPrefixUnaryExpression(parent)) {
    return parent.getOperatorToken() === SyntaxKind.ExclamationToken;
  }
  if (Node.isIfStatement(parent)) return parent.getExpression() === node;
  if (Node.isConditionalExpression(parent)) return parent.getCondition() === node;
  if (Node.isSpreadAssignment(parent)) return true;
  if (Node.isVariableDeclaration(parent) && parent.getInitializer() === node) {
    const name = parent.getNameNode();
    return Node.isIdentifier(name) && nameToleratesAbsence(name, depth + 1);
  }
  return false;
}

/**
 * A binding that receives the body, or a payload, tolerates `undefined`: it
 * has a default, or it binds a name whose every read tolerates it. A pattern
 * with no default destructures `undefined` and throws.
 */
export function bindingToleratesAbsence(binding: ParameterDeclaration | BindingElement): boolean {
  if (binding.hasInitializer()) return true;
  const name = binding.getNameNode();
  return Node.isIdentifier(name) && nameToleratesAbsence(name, 0);
}

/** The name a binding element takes out of its object: `body` in `{ body }` and `{ body: b }`. */
function memberNameOf(element: BindingElement): string {
  return element.getPropertyNameNode()?.getText() ?? element.getName();
}

/** The root identifier of a member chain (`ctx` in `ctx.request.body`), through transparent wrappers. */
function rootOf(expression: Node): Node {
  let root = expression;
  while (
    Node.isPropertyAccessExpression(root) ||
    Node.isElementAccessExpression(root) ||
    Node.isNonNullExpression(root) ||
    Node.isParenthesizedExpression(root)
  ) {
    root = root.getExpression();
  }
  return root;
}

/**
 * True when the handler reads its request body, and every read tolerates the
 * body being absent. False when it reads none: a body read some other way
 * (handed on with the request, read by a method) cannot be seen here.
 *
 * A body read is the `body` member of a value rooted at one of the handler's
 * parameters (`request.body`, `ctx.request.body`), or a binding that takes
 * `body` out of a parameter or out of such a value (`({ body }) =>`,
 * `const { body } = request`). A parameter the body is read through must not
 * be handed on whole (`service.create(request)`): what the callee reads off
 * it is not seen.
 */
export function handlerToleratesAbsentBody(handler: HandlerFunction): boolean {
  const parameters = new Set<Node>(handler.getParameters());
  const parameterOf = (identifier: Node): ParameterDeclaration | undefined => {
    if (!Node.isIdentifier(identifier)) return undefined;
    for (const declaration of identifier.getSymbol()?.getDeclarations() ?? []) {
      const parameter = Node.isParameterDeclaration(declaration)
        ? declaration
        : Node.isBindingElement(declaration)
          ? declaration.getFirstAncestorByKind(SyntaxKind.Parameter)
          : undefined;
      if (parameter && parameters.has(parameter)) return parameter;
    }
    return undefined;
  };

  let reads = 0;
  const readThrough = new Set<ParameterDeclaration>();

  for (const parameter of handler.getParameters()) {
    for (const element of parameter.getDescendantsOfKind(SyntaxKind.BindingElement)) {
      if (memberNameOf(element) !== BODY_MEMBER) continue;
      reads++;
      if (!bindingToleratesAbsence(element)) return false;
    }
  }

  for (const access of handler.getDescendantsOfKind(SyntaxKind.PropertyAccessExpression)) {
    if (access.getName() !== BODY_MEMBER) continue;
    const parameter = parameterOf(rootOf(access.getExpression()));
    if (!parameter) continue;
    reads++;
    readThrough.add(parameter);
    if (!valueToleratesAbsence(access, 0)) return false;
  }

  for (const declaration of handler.getDescendantsOfKind(SyntaxKind.VariableDeclaration)) {
    const pattern = declaration.getNameNode();
    const initializer = declaration.getInitializer();
    if (!Node.isObjectBindingPattern(pattern) || !initializer) continue;
    const parameter = parameterOf(rootOf(initializer));
    if (!parameter) continue;
    for (const element of pattern.getElements()) {
      if (memberNameOf(element) !== BODY_MEMBER) continue;
      reads++;
      readThrough.add(parameter);
      if (!bindingToleratesAbsence(element)) return false;
    }
  }

  for (const parameter of readThrough) {
    const name = parameter.getNameNode();
    if (!Node.isIdentifier(name)) continue;
    const symbol = name.getSymbol();
    for (const reference of handler.getDescendantsOfKind(SyntaxKind.Identifier)) {
      if (reference === name || reference.getSymbol() !== symbol) continue;
      const parent = reference.getParent();
      const memberRead =
        parent !== undefined &&
        (Node.isPropertyAccessExpression(parent) || Node.isElementAccessExpression(parent)) &&
        parent.getExpression() === reference;
      const destructured =
        parent !== undefined &&
        Node.isVariableDeclaration(parent) &&
        parent.getInitializer() === reference &&
        Node.isObjectBindingPattern(parent.getNameNode());
      if (!memberRead && !destructured) return false;
    }
  }

  return reads > 0;
}
