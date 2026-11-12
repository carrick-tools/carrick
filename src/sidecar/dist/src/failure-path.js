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
 * The retype check (retype.ts) reads the same tests through `testsOnPath` and
 * keeps its own policy on top: it also reads the false side of
 * `res.status === 200` as the failure path, so it can find a success read to
 * judge. Here a decided failure REMOVES a read, so the reading has to be sound
 * the other way round: after `if (res.status === 204) return null`, 200 still
 * gets through, and the json read that follows is the payload.
 */
import { Node, SyntaxKind } from 'ts-morph';
const FIRST_STATUS = 100;
const LAST_STATUS = 599;
const EVERY_STATUS = () => true;
/** The statuses `ok` is true for. */
export const SUCCEEDED = (status) => status >= 200 && status <= 299;
/** The statuses from 100 to 599 that `statuses` admits, in order. */
export function statusesIn(statuses) {
    const admitted = [];
    for (let status = FIRST_STATUS; status <= LAST_STATUS; status++) {
        if (statuses(status))
            admitted.push(status);
    }
    return admitted;
}
/** A condition that does not read the response's `ok` or `status`. */
const UNRELATED = { whenTrue: EVERY_STATUS, whenFalse: EVERY_STATUS, inexact: false };
/** A condition that reads the response's status in a form this reading cannot follow. */
const UNREAD = { whenTrue: EVERY_STATUS, whenFalse: EVERY_STATUS, inexact: true };
/**
 * True when the source reaches `node` only after the response named by
 * `isResponse` failed. The walk climbs from `node` to `boundary` (the function
 * the call sits in) and never looks at a test outside it.
 */
export function reachedOnlyOnFailure(node, boundary, isResponse) {
    const sides = testsOnPath(node, boundary, isResponse);
    if (sides.length === 0)
        return false;
    const reaching = statusesIn((status) => sides.every((side) => side.admits(status)));
    return reaching.length > 0 && !reaching.some(SUCCEEDED);
}
/**
 * Each test of the response on the path from `node` up to `boundary` (the
 * whole file when it is `undefined`), as the side `node` is on: a branch of
 * an `if` or a conditional expression it sits in, or an earlier `if` in an
 * enclosing block one of whose branches cannot complete, which leaves the
 * rest of the block to the other side. A test that does not read the
 * response's `ok` or `status` is not listed. A `switch` is not read here:
 * `retype.ts` finds one by its own climb.
 */
export function testsOnPath(node, boundary, isResponse) {
    const sides = [];
    for (const step of stepsOnPath(node, boundary)) {
        if (step.kind !== 'branch')
            continue;
        const test = readTest(step.condition, isResponse);
        if (test === UNRELATED)
            continue;
        sides.push({ admits: step.whenTrue ? test.whenTrue : test.whenFalse, inexact: test.inexact });
    }
    return sides;
}
/**
 * Every test on the path from `node` up to `boundary` (the whole file when it
 * is `undefined`), whatever it reads, innermost first. `testsOnPath` reads
 * the branches as statuses; `response-modes.ts` reads branches and switches
 * as values of a request field (carrick#2054).
 */
export function stepsOnPath(node, boundary) {
    const steps = [];
    const branch = (test, condition, whenTrue) => steps.push({ kind: 'branch', test, condition, whenTrue });
    for (let child = node, parent = node.getParent(); parent && parent !== boundary; child = parent, parent = parent.getParent()) {
        if (Node.isIfStatement(parent)) {
            if (child === parent.getThenStatement()) {
                branch(parent, parent.getExpression(), true);
            }
            else if (child === parent.getElseStatement()) {
                branch(parent, parent.getExpression(), false);
            }
        }
        else if (Node.isConditionalExpression(parent)) {
            if (child === parent.getWhenTrue()) {
                branch(parent, parent.getCondition(), true);
            }
            else if (child === parent.getWhenFalse()) {
                branch(parent, parent.getCondition(), false);
            }
        }
        else if (Node.isCaseClause(parent) || Node.isDefaultClause(parent)) {
            const statement = parent.getParent()?.getParent();
            if (statement && Node.isSwitchStatement(statement)) {
                steps.push({ kind: 'clause', test: statement, clause: parent });
            }
        }
        if (Node.isBlock(parent) ||
            Node.isSourceFile(parent) ||
            Node.isCaseClause(parent) ||
            Node.isDefaultClause(parent)) {
            for (const statement of parent.getStatements()) {
                if (statement === child)
                    break;
                if (Node.isSwitchStatement(statement)) {
                    steps.push({ kind: 'after-switch', test: statement });
                    continue;
                }
                if (!Node.isIfStatement(statement))
                    continue;
                const otherwise = statement.getElseStatement();
                const thenLeaves = cannotComplete(statement.getThenStatement());
                const elseLeaves = otherwise !== undefined && cannotComplete(otherwise);
                if (thenLeaves && !elseLeaves) {
                    branch(statement, statement.getExpression(), false);
                }
                else if (elseLeaves && !thenLeaves) {
                    branch(statement, statement.getExpression(), true);
                }
            }
        }
    }
    return steps;
}
/** The statuses `condition` lets through when it is true and when it is false. */
function readTest(condition, isResponse) {
    let test = condition;
    while (Node.isParenthesizedExpression(test))
        test = test.getExpression();
    if (Node.isPrefixUnaryExpression(test) &&
        test.getOperatorToken() === SyntaxKind.ExclamationToken) {
        const inner = readTest(test.getOperand(), isResponse);
        if (inner === UNRELATED || inner === UNREAD)
            return inner;
        return { whenTrue: inner.whenFalse, whenFalse: inner.whenTrue, inexact: inner.inexact };
    }
    if (isMemberOfResponse(test, 'ok', isResponse)) {
        return { whenTrue: SUCCEEDED, whenFalse: (status) => !SUCCEEDED(status), inexact: false };
    }
    if (Node.isBinaryExpression(test)) {
        const operator = test.getOperatorToken().getKind();
        if (operator === SyntaxKind.AmpersandAmpersandToken ||
            operator === SyntaxKind.BarBarToken) {
            const left = readTest(test.getLeft(), isResponse);
            const right = readTest(test.getRight(), isResponse);
            if (left === UNRELATED && right === UNRELATED)
                return UNRELATED;
            const inexact = left.inexact || right.inexact || left === UNRELATED || right === UNRELATED;
            return operator === SyntaxKind.AmpersandAmpersandToken
                ? {
                    whenTrue: (status) => left.whenTrue(status) && right.whenTrue(status),
                    whenFalse: (status) => left.whenFalse(status) || right.whenFalse(status),
                    inexact,
                }
                : {
                    whenTrue: (status) => left.whenTrue(status) || right.whenTrue(status),
                    whenFalse: (status) => left.whenFalse(status) && right.whenFalse(status),
                    inexact,
                };
        }
        const compared = statusComparison(test.getLeft(), operator, test.getRight(), isResponse);
        if (compared) {
            return { whenTrue: compared, whenFalse: (status) => !compared(status), inexact: false };
        }
    }
    return readsResponseStatus(test, isResponse) ? UNREAD : UNRELATED;
}
/**
 * `res.status <op> N` or `N <op> res.status`, as the statuses it is true for;
 * `undefined` when the comparison is not one of the response's status with a
 * number.
 */
function statusComparison(left, operator, right, isResponse) {
    let op = operator;
    let value;
    if (isMemberOfResponse(left, 'status', isResponse) && Node.isNumericLiteral(right)) {
        value = right.getLiteralValue();
    }
    else if (isMemberOfResponse(right, 'status', isResponse) && Node.isNumericLiteral(left)) {
        value = left.getLiteralValue();
        op = MIRRORED[op] ?? op;
    }
    else {
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
const MIRRORED = {
    [SyntaxKind.LessThanToken]: SyntaxKind.GreaterThanToken,
    [SyntaxKind.LessThanEqualsToken]: SyntaxKind.GreaterThanEqualsToken,
    [SyntaxKind.GreaterThanToken]: SyntaxKind.LessThanToken,
    [SyntaxKind.GreaterThanEqualsToken]: SyntaxKind.LessThanEqualsToken,
};
function isMemberOfResponse(node, name, isResponse) {
    return (Node.isPropertyAccessExpression(node) &&
        node.getName() === name &&
        isResponse(node.getExpression()));
}
/** `node` reads the response's `ok` or `status`, itself or anywhere inside it. */
export function readsResponseStatus(node, isResponse) {
    return [node, ...node.getDescendantsOfKind(SyntaxKind.PropertyAccessExpression)].some((inner) => isMemberOfResponse(inner, 'ok', isResponse) ||
        isMemberOfResponse(inner, 'status', isResponse));
}
/**
 * A statement that never runs on into the statement after it: it returns,
 * throws, breaks or continues, or every path through it does. A loop, a
 * `switch` or a `try` is read as one that can complete, which only ever
 * leaves a read on the path it was on.
 */
export function cannotComplete(statement) {
    if (Node.isReturnStatement(statement) ||
        Node.isThrowStatement(statement) ||
        Node.isBreakStatement(statement) ||
        Node.isContinueStatement(statement)) {
        return true;
    }
    if (Node.isBlock(statement)) {
        return statement.getStatements().some((inner) => cannotComplete(inner));
    }
    if (Node.isIfStatement(statement)) {
        const otherwise = statement.getElseStatement();
        return (otherwise !== undefined &&
            cannotComplete(statement.getThenStatement()) &&
            cannotComplete(otherwise));
    }
    return false;
}
