/**
 * A request body written as a conditional over the serialiser
 * (`body: x ? JSON.stringify(x) : null`), carrick#4285.
 *
 * A direct `JSON.stringify(x)` body reads as the type of `x`: the call's own
 * `string` is the wire text, never a contract. A conditional body reads each
 * arm the same way: a serialised arm as its argument's type, any other arm as
 * its own, and the body is the union of the arms. A conditional with no
 * serialised arm is none of this module's business and reads as it always did.
 *
 * Lives in the capture bundle; the v1 inferrer (ts-morph) reaches it through
 * capture/index.js, and the capture anchor
 * resolver (raw program) answer it with the same rule.
 */

import ts from 'typescript';

const MAX_DEPTH = 8;

function peel(node: ts.Node): ts.Node {
  let current = node;
  while (ts.isParenthesizedExpression(current)) current = current.expression;
  return current;
}

/** The argument of a `JSON.stringify(arg)` call, else undefined. */
function stringifiedArgument(node: ts.Node): ts.Node | undefined {
  if (
    ts.isCallExpression(node) &&
    node.arguments.length > 0 &&
    ts.isPropertyAccessExpression(node.expression) &&
    ts.isIdentifier(node.expression.expression) &&
    node.expression.expression.text === 'JSON' &&
    node.expression.name.text === 'stringify'
  ) {
    return node.arguments[0];
  }
  return undefined;
}

/**
 * The nodes whose types make up a conditional serialised body: for each arm,
 * the serialiser's argument or the arm itself (a nested conditional is
 * flattened). Undefined when `node` is not a conditional, or when none of its
 * arms is a `JSON.stringify` call. A property assignment reads as its value.
 */
export function serialisedConditionalArms(node: ts.Node): ts.Node[] | undefined {
  const start = peel(ts.isPropertyAssignment(node) ? node.initializer : node);
  if (!ts.isConditionalExpression(start)) return undefined;
  const arms: ts.Node[] = [];
  let serialised = false;
  const walk = (expression: ts.Node, depth: number): void => {
    const arm = peel(expression);
    if (ts.isConditionalExpression(arm) && depth < MAX_DEPTH) {
      walk(arm.whenTrue, depth + 1);
      walk(arm.whenFalse, depth + 1);
      return;
    }
    const argument = stringifiedArgument(arm);
    if (argument) serialised = true;
    arms.push(argument ? peel(argument) : arm);
  };
  walk(start.whenTrue, 0);
  walk(start.whenFalse, 0);
  return serialised ? arms : undefined;
}

/**
 * The union of the arms' own types, or undefined when the checker cannot join
 * them (the caller then keeps its existing reading).
 */
export function serialisedConditionalType(
  checker: ts.TypeChecker,
  node: ts.Node
): ts.Type | undefined {
  const arms = serialisedConditionalArms(node);
  if (!arms) return undefined;
  const types = arms.map((arm) => checker.getTypeAtLocation(arm));
  if (types.length === 1) return types[0];
  const union = (
    checker as unknown as { getUnionType?: (types: ts.Type[], reduction?: number) => ts.Type }
  ).getUnionType;
  return union ? union.call(checker, types, 1 /* UnionReduction.Literal */) : undefined;
}
