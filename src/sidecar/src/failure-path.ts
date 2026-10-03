/**
 * Whether the source reaches a node only when an HTTP response FAILED
 * (carrick#1796).
 *
 * A consumer reads the success body on one path and the error text or the
 * error body on another. Only the first is the call's response contract, so
 * the def-use walk behind a `call_result` row must not take a read from the
 * second. Which path a read is on is told by the source's own tests of the
 * response, read two ways:
 *
 *  - the read sits in a branch of a test: `if (!res.ok) { ... }`, the `else`
 *    of `if (res.ok)`, either arm of a conditional expression;
 *  - the read follows an `if` whose other branch cannot complete, so the rest
 *    of the block runs only on the side that did not leave:
 *    `if (res.ok) { return ... } const text = await res.text()`.
 *
 * A test is read as the set of statuses it lets through. `ok` is true for
 * 200-299 and false for every other status; a comparison of the status with a
 * number lets through what it admits; `!`, `&&` and `||` combine the sets, and
 * any other condition lets everything through. A node is on the failure path
 * only when NO status in 200-299 can reach it.
 *
 * That is deliberately stricter than the retype check's reading of a status
 * test (`okWhenTrue` in retype.ts), which reads the false side of
 * `res.status === 200` as the failure path so it can find a success read to
 * judge. Here a decided failure REMOVES a read, so the reading has to be sound
 * the other way round: after `if (res.status === 204) return null`, 200 still
 * gets through, and the json read that follows is the payload.
 */

import { Node, SyntaxKind } from 'ts-morph';

/** A set of HTTP statuses, as the predicate that admits them. */
type Statuses = (status: number) => boolean;

const FIRST_STATUS = 100;
const LAST_STATUS = 599;

const EVERY_STATUS: Statuses = () => true;

/** The statuses `ok` is true for. */
const SUCCEEDED: Statuses = (status) => status >= 200 && status <= 299;

/** The statuses a condition lets through on each of its sides. */
interface StatusTest {
  whenTrue: Statuses;
  whenFalse: Statuses;
}

const UNDECIDED: StatusTest = { whenTrue: EVERY_STATUS, whenFalse: EVERY_STATUS };

/**
 * True when the source reaches `node` only after the response named by
 * `isResponse` failed. The walk climbs from `node` to `boundary` (the function
 * the call sits in) and never looks at a test outside it.
 */
export function reachedOnlyOnFailure(
  node: Node,
  boundary: Node,
  isResponse: (node: Node) => boolean
): boolean {
  let admitted: Statuses = EVERY_STATUS;
  let narrowed = false;
  const narrow = (by: Statuses) => {
    if (by === EVERY_STATUS) return;
    const before = admitted;
    admitted = (status) => before(status) && by(status);
    narrowed = true;
  };

  for (
    let child: Node = node, parent = node.getParent();
    parent && parent !== boundary;
    child = parent, parent = parent.getParent()
  ) {
    if (Node.isIfStatement(parent)) {
      if (child === parent.getThenStatement()) {
        narrow(readTest(parent.getExpression(), isResponse).whenTrue);
      } else if (child === parent.getElseStatement()) {
        narrow(readTest(parent.getExpression(), isResponse).whenFalse);
      }
    } else if (Node.isConditionalExpression(parent)) {
      if (child === parent.getWhenTrue()) {
        narrow(readTest(parent.getCondition(), isResponse).whenTrue);
      } else if (child === parent.getWhenFalse()) {
        narrow(readTest(parent.getCondition(), isResponse).whenFalse);
      }
    }

    if (
      Node.isBlock(parent) ||
      Node.isSourceFile(parent) ||
      Node.isCaseClause(parent) ||
      Node.isDefaultClause(parent)
    ) {
      for (const statement of parent.getStatements()) {
        if (statement === child) break;
        if (!Node.isIfStatement(statement)) continue;
        const otherwise = statement.getElseStatement();
        const thenLeaves = cannotComplete(statement.getThenStatement());
        const elseLeaves = otherwise !== undefined && cannotComplete(otherwise);
        if (thenLeaves && !elseLeaves) {
          narrow(readTest(statement.getExpression(), isResponse).whenFalse);
        } else if (elseLeaves && !thenLeaves) {
          narrow(readTest(statement.getExpression(), isResponse).whenTrue);
        }
      }
    }
  }

  if (!narrowed) return false;
  let reachable = false;
  for (let status = FIRST_STATUS; status <= LAST_STATUS; status++) {
    if (!admitted(status)) continue;
    if (SUCCEEDED(status)) return false;
    reachable = true;
  }
  return reachable;
}

/** The statuses `condition` lets through when it is true and when it is false. */
function readTest(condition: Node, isResponse: (node: Node) => boolean): StatusTest {
  let test = condition;
  while (Node.isParenthesizedExpression(test)) test = test.getExpression();

  if (
    Node.isPrefixUnaryExpression(test) &&
    test.getOperatorToken() === SyntaxKind.ExclamationToken
  ) {
    const inner = readTest(test.getOperand(), isResponse);
    return { whenTrue: inner.whenFalse, whenFalse: inner.whenTrue };
  }

  if (isMemberOfResponse(test, 'ok', isResponse)) {
    return { whenTrue: SUCCEEDED, whenFalse: (status) => !SUCCEEDED(status) };
  }

  if (Node.isBinaryExpression(test)) {
    const operator = test.getOperatorToken().getKind();
    if (
      operator === SyntaxKind.AmpersandAmpersandToken ||
      operator === SyntaxKind.BarBarToken
    ) {
      const left = readTest(test.getLeft(), isResponse);
      const right = readTest(test.getRight(), isResponse);
      if (left === UNDECIDED && right === UNDECIDED) return UNDECIDED;
      return operator === SyntaxKind.AmpersandAmpersandToken
        ? {
            whenTrue: (status) => left.whenTrue(status) && right.whenTrue(status),
            whenFalse: (status) => left.whenFalse(status) || right.whenFalse(status),
          }
        : {
            whenTrue: (status) => left.whenTrue(status) || right.whenTrue(status),
            whenFalse: (status) => left.whenFalse(status) && right.whenFalse(status),
          };
    }

    const compared = statusComparison(test.getLeft(), operator, test.getRight(), isResponse);
    if (compared) {
      return { whenTrue: compared, whenFalse: (status) => !compared(status) };
    }
  }

  return UNDECIDED;
}

/**
 * `res.status <op> N` or `N <op> res.status`, as the statuses it is true for;
 * `undefined` when the comparison is not one of the response's status with a
 * number.
 */
function statusComparison(
  left: Node,
  operator: SyntaxKind,
  right: Node,
  isResponse: (node: Node) => boolean
): Statuses | undefined {
  let op = operator;
  let value: number;
  if (isMemberOfResponse(left, 'status', isResponse) && Node.isNumericLiteral(right)) {
    value = right.getLiteralValue();
  } else if (isMemberOfResponse(right, 'status', isResponse) && Node.isNumericLiteral(left)) {
    value = left.getLiteralValue();
    op = MIRRORED[op] ?? op;
  } else {
    return undefined;
  }
  switch (op) {
    case SyntaxKind.EqualsEqualsEqualsToken:
    case SyntaxKind.EqualsEqualsToken:
      return (status) => status === value;
    case SyntaxKind.ExclamationEqualsEqualsToken:
    case SyntaxKind.ExclamationEqualsToken:
      return (status) => status !== value;
    case SyntaxKind.LessThanToken:
      return (status) => status < value;
    case SyntaxKind.LessThanEqualsToken:
      return (status) => status <= value;
    case SyntaxKind.GreaterThanToken:
      return (status) => status > value;
    case SyntaxKind.GreaterThanEqualsToken:
      return (status) => status >= value;
    default:
      return undefined;
  }
}

/** `400 <= res.status` reads as `res.status >= 400`. */
const MIRRORED: Partial<Record<SyntaxKind, SyntaxKind>> = {
  [SyntaxKind.LessThanToken]: SyntaxKind.GreaterThanToken,
  [SyntaxKind.LessThanEqualsToken]: SyntaxKind.GreaterThanEqualsToken,
  [SyntaxKind.GreaterThanToken]: SyntaxKind.LessThanToken,
  [SyntaxKind.GreaterThanEqualsToken]: SyntaxKind.LessThanEqualsToken,
};

function isMemberOfResponse(
  node: Node,
  name: string,
  isResponse: (node: Node) => boolean
): boolean {
  return (
    Node.isPropertyAccessExpression(node) &&
    node.getName() === name &&
    isResponse(node.getExpression())
  );
}

/**
 * A statement that never runs on into the statement after it: it returns,
 * throws, breaks or continues, or every path through it does. A loop, a
 * `switch` or a `try` is read as one that can complete, which only ever
 * leaves a read on the path it was on.
 */
function cannotComplete(statement: Node): boolean {
  if (
    Node.isReturnStatement(statement) ||
    Node.isThrowStatement(statement) ||
    Node.isBreakStatement(statement) ||
    Node.isContinueStatement(statement)
  ) {
    return true;
  }
  if (Node.isBlock(statement)) {
    return statement.getStatements().some((inner) => cannotComplete(inner));
  }
  if (Node.isIfStatement(statement)) {
    const otherwise = statement.getElseStatement();
    return (
      otherwise !== undefined &&
      cannotComplete(statement.getThenStatement()) &&
      cannotComplete(otherwise)
    );
  }
  return false;
}
