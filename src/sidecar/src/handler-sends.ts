/**
 * What a function does with the parameters it was handed (carrick#1913).
 *
 * A route handler that returns nothing answers through one of its parameters:
 * `res.status(201).json(created)`. Reading that needs three facts about the
 * source, none of which is a name:
 *
 *  - every place the handler's body uses a parameter, sorted by HOW it uses
 *    it: a method called directly on it (or at the end of a chain of calls on
 *    it), a call of the parameter itself, the parameter handed to another
 *    call, a member of it assigned, a use inside a function the handler
 *    declares, or the parameter kept as a value;
 *  - whether one of those is the last thing done with the parameters on the
 *    path it is on (`lastOnItsPath`);
 *  - whether it sits in a catch clause (`inCatchClause`), which answers a
 *    failure.
 *
 * This module reads only syntax and symbol identity. What a use MEANS (which
 * parameter is transport, which call takes a body, what status it states) is
 * the inferrer's to decide, with the type tests it already owns.
 */

import {
  Node,
  SyntaxKind,
  type ArrowFunction,
  type BinaryExpression,
  type CallExpression,
  type FunctionDeclaration,
  type FunctionExpression,
  type MethodDeclaration,
} from 'ts-morph';

export type HandlerFunction =
  | FunctionDeclaration
  | ArrowFunction
  | FunctionExpression
  | MethodDeclaration;

export function isHandlerFunction(node: Node | undefined): node is HandlerFunction {
  return (
    node !== undefined &&
    (Node.isFunctionDeclaration(node) ||
      Node.isArrowFunction(node) ||
      Node.isFunctionExpression(node) ||
      Node.isMethodDeclaration(node))
  );
}

/** A method called directly on a parameter, or at the end of a chain of such calls. */
export interface DirectCall {
  /** The parameter, or the binding element of a destructured one, the chain starts at. */
  parameter: Node;
  /** The last call of the chain: in `res.status(201).json(body)`, the `json` call. */
  call: CallExpression;
  /** The calls before it in the chain, innermost first. */
  earlier: CallExpression[];
  /**
   * True when the chain's value is discarded or returned, which is how a send
   * is written. A chain whose value is used (`res.getHeader('etag')` bound to
   * a name, or handed on) reads the parameter.
   */
  discarded: boolean;
}

/** A parameter handed to a call. */
export interface HandOff {
  parameter: Node;
  call: Node;
  /** Which argument of the call it is, or is held by. */
  argument: number;
  /** The member it is the value of, when handed over inside an object literal. */
  member?: string;
}

export interface ParameterUses {
  /** Every parameter the handler declares, a destructured one as its elements. */
  parameters: Node[];
  /** Methods called directly on a parameter, in source order. */
  calls: DirectCall[];
  /** Calls OF a parameter: `next(error)`. */
  invocations: Array<{ parameter: Node; call: CallExpression }>;
  /**
   * Calls a parameter is handed to: as an argument (`respond(res, body)`), or
   * as a member of an object literal that is one (`finish({ res, body })`).
   */
  handOffs: HandOff[];
  /** Assignments to a member of a parameter: `res.statusCode = 404`. */
  assignments: Array<{ parameter: Node; assignment: BinaryExpression }>;
  /**
   * A parameter ACTED on inside a function the handler declares: a method
   * called on it and its value discarded, a hand-off, an assignment, or the
   * parameter kept. A read there (`res.statusCode`, a call whose value is
   * used) is not listed: it sends nothing.
   */
  nested: Array<{ parameter: Node; at: Node }>;
  /** A parameter kept as a value: bound to a name, returned, spread. */
  kept: Array<{ parameter: Node; at: Node }>;
}

/** Wrappers that leave the value they hold as it is. */
function isTransparent(node: Node): boolean {
  return (
    Node.isParenthesizedExpression(node) ||
    Node.isNonNullExpression(node) ||
    Node.isAsExpression(node) ||
    Node.isSatisfiesExpression(node) ||
    Node.isTypeAssertion(node)
  );
}

/** The outermost transparent wrapper around `node`, or `node`. */
function wrapped(node: Node): Node {
  let current = node;
  for (let parent = current.getParent(); parent && isTransparent(parent); parent = current.getParent()) {
    current = parent;
  }
  return current;
}

function isAssignment(expression: BinaryExpression): boolean {
  const operator = expression.getOperatorToken().getKind();
  return operator >= SyntaxKind.FirstAssignment && operator <= SyntaxKind.LastAssignment;
}

/** The call `member` is the callee of, when it is one. */
function callOf(member: Node): CallExpression | undefined {
  const callee = wrapped(member);
  const parent = callee.getParent();
  return Node.isCallExpression(parent) && parent.getExpression() === callee ? parent : undefined;
}

/** The member access `value` is the receiver of, when it is one. */
function memberOn(value: Node): Node | undefined {
  const receiver = wrapped(value);
  const parent = receiver.getParent();
  return (Node.isPropertyAccessExpression(parent) || Node.isElementAccessExpression(parent)) &&
    parent.getExpression() === receiver
    ? parent
    : undefined;
}

/**
 * True when the value of `call` is discarded or returned: it is a statement
 * of its own, a returned expression, the concise body of `owner` (the
 * function that holds it), or a branch of a conditional or of `&&` / `||` /
 * `??` / `,` that is.
 */
function isDiscarded(call: Node, owner: Node | undefined): boolean {
  let value: Node = call;
  for (;;) {
    const parent = value.getParent();
    if (!parent) return false;
    if (
      isTransparent(parent) ||
      Node.isAwaitExpression(parent) ||
      Node.isVoidExpression(parent)
    ) {
      value = parent;
      continue;
    }
    if (Node.isConditionalExpression(parent) && parent.getCondition() !== value) {
      value = parent;
      continue;
    }
    if (Node.isBinaryExpression(parent)) {
      const operator = parent.getOperatorToken().getKind();
      if (
        operator === SyntaxKind.AmpersandAmpersandToken ||
        operator === SyntaxKind.BarBarToken ||
        operator === SyntaxKind.QuestionQuestionToken ||
        operator === SyntaxKind.CommaToken
      ) {
        value = parent;
        continue;
      }
      return false;
    }
    if (Node.isExpressionStatement(parent) || Node.isReturnStatement(parent)) return true;
    return parent === owner;
  }
}

/**
 * The call `value` is handed to: the call it is an argument of, or the call
 * an object literal that holds it as a member's value is an argument of.
 */
function handedTo(value: Node): Omit<HandOff, 'parameter'> | undefined {
  let handed = value;
  let member: string | undefined;
  const holder = handed.getParent();
  if (
    (Node.isShorthandPropertyAssignment(holder) && holder.getNameNode() === handed) ||
    (Node.isPropertyAssignment(holder) && holder.getInitializer() === handed)
  ) {
    const literal = holder.getParent();
    if (!literal || !Node.isObjectLiteralExpression(literal)) return undefined;
    member = holder.getName();
    handed = wrapped(literal);
  }
  const call = handed.getParent();
  if (!call || (!Node.isCallExpression(call) && !Node.isNewExpression(call))) return undefined;
  const argument = call.getArguments().indexOf(handed);
  return argument >= 0 ? { call, argument, member } : undefined;
}

/** Every use the body of `handler` makes of the parameters it declares. */
export function parameterUses(handler: HandlerFunction): ParameterUses {
  const uses: ParameterUses = {
    parameters: [],
    calls: [],
    invocations: [],
    handOffs: [],
    assignments: [],
    nested: [],
    kept: [],
  };
  const declared = new Set<Node>();
  const declare = (name: Node): void => {
    if (Node.isIdentifier(name)) {
      const declaration = name.getParent();
      if (declaration) declared.add(declaration);
      return;
    }
    if (Node.isObjectBindingPattern(name) || Node.isArrayBindingPattern(name)) {
      for (const element of name.getElements()) {
        if (Node.isBindingElement(element)) declare(element.getNameNode());
      }
    }
  };
  for (const parameter of handler.getParameters()) declare(parameter.getNameNode());
  uses.parameters = [...declared];
  const body = handler.getBody();
  if (!body || declared.size === 0) return uses;

  const identifiers = Node.isIdentifier(body)
    ? [body]
    : body.getDescendantsOfKind(SyntaxKind.Identifier);
  for (const identifier of identifiers) {
    const holder = identifier.getParent();
    const symbol = Node.isShorthandPropertyAssignment(holder)
      ? holder.getValueSymbol()
      : identifier.getSymbol();
    const parameter = (symbol?.getDeclarations() ?? []).find((declaration) =>
      declared.has(declaration)
    );
    if (!parameter) continue;
    // `typeof res` in a type position reads nothing at run time.
    if (identifier.getFirstAncestorByKind(SyntaxKind.TypeQuery)) continue;

    // A use inside a function the handler declares is listed only when it
    // acts on the parameter; `own` is false for it.
    const owner = identifier.getFirstAncestor(isHandlerFunction);
    const own = owner === handler;
    const acted = (): void => {
      uses.nested.push({ parameter, at: identifier });
    };

    const member = memberOn(identifier);
    if (member) {
      let call = callOf(member);
      if (call) {
        const earlier: CallExpression[] = [];
        for (;;) {
          const next = memberOn(call);
          const nextCall = next ? callOf(next) : undefined;
          if (!nextCall) break;
          earlier.push(call);
          call = nextCall;
        }
        const discarded = isDiscarded(call, owner);
        if (own) uses.calls.push({ parameter, call, earlier, discarded });
        else if (discarded) acted();
        continue;
      }
      const target = wrapped(member);
      const assignment = target.getParent();
      if (
        Node.isBinaryExpression(assignment) &&
        assignment.getLeft() === target &&
        isAssignment(assignment)
      ) {
        if (own) uses.assignments.push({ parameter, assignment });
        else acted();
      }
      // Any other member use reads the parameter, or calls an object it holds.
      continue;
    }

    const value = wrapped(identifier);
    const parent = value.getParent();
    if (Node.isCallExpression(parent) && parent.getExpression() === value) {
      if (own) uses.invocations.push({ parameter, call: parent });
      else acted();
      continue;
    }
    const receiver = handedTo(value);
    if (receiver) {
      if (own) uses.handOffs.push({ parameter, ...receiver });
      else acted();
      continue;
    }
    // A test of the parameter reads it without keeping it.
    if (
      Node.isTypeOfExpression(parent) ||
      Node.isPrefixUnaryExpression(parent) ||
      (Node.isIfStatement(parent) && parent.getExpression() === value)
    ) {
      continue;
    }
    if (own) uses.kept.push({ parameter, at: identifier });
    else acted();
  }
  return uses;
}

function isStatementList(
  node: Node
): node is Node & { getStatements(): Node[] } {
  return (
    Node.isBlock(node) ||
    Node.isSourceFile(node) ||
    Node.isCaseClause(node) ||
    Node.isDefaultClause(node) ||
    Node.isModuleBlock(node)
  );
}

function isLoop(node: Node): boolean {
  return (
    Node.isForStatement(node) ||
    Node.isForInStatement(node) ||
    Node.isForOfStatement(node) ||
    Node.isWhileStatement(node) ||
    Node.isDoStatement(node)
  );
}

const inside = (inner: Node, outer: Node): boolean =>
  outer.getStart() <= inner.getStart() && inner.getEnd() <= outer.getEnd();

/**
 * True when control cannot run on past `statement` inside the function: it
 * returns or throws, on every branch it has. A `break` or a `continue` leaves
 * a loop or a `switch` and not the function, so what follows that loop still
 * runs; this is why `cannotComplete` in `failure-path.ts`, which counts both,
 * is not the test here.
 */
function leavesTheFunction(statement: Node): boolean {
  if (Node.isReturnStatement(statement) || Node.isThrowStatement(statement)) return true;
  if (Node.isBlock(statement)) {
    return statement.getStatements().some((inner) => leavesTheFunction(inner));
  }
  if (Node.isIfStatement(statement)) {
    const otherwise = statement.getElseStatement();
    return (
      otherwise !== undefined &&
      leavesTheFunction(statement.getThenStatement()) &&
      leavesTheFunction(otherwise)
    );
  }
  return false;
}

/**
 * The statement of `handler`'s own body that holds `node`, or `undefined`
 * for a concise arrow body, which is one expression and holds no statement.
 */
function statementHolding(node: Node, handler: HandlerFunction): Node | undefined {
  const body = handler.getBody();
  if (!body || !Node.isBlock(body)) return undefined;
  const statement = node.getFirstAncestor((ancestor) => Node.isStatement(ancestor));
  return statement && statement !== body && inside(statement, body) ? statement : undefined;
}

/**
 * True when none of `acts` can run after `node` on the path `node` is on.
 *
 * Read from the statements alone. Starting at the statement that holds
 * `node`, every statement after it in the same list is on its path until one
 * that leaves the function (a `return`, a `throw`); then the same is asked of the
 * statement that holds that list, out to the handler's own body. A loop runs
 * its body again, so nothing in one is last. A `finally` block runs after the
 * `try` it closes. A `catch` clause does not follow the `try` on the path
 * where the `try` completed, and is not read as following it.
 */
export function lastOnItsPath(node: Node, handler: HandlerFunction, acts: Node[]): boolean {
  let child = statementHolding(node, handler);
  if (!child) return !acts.some((act) => act.getStart() >= node.getEnd());
  // Later in the same statement: `res.json(a), next()`.
  const statement = child;
  if (acts.some((act) => act.getStart() >= node.getEnd() && inside(act, statement))) {
    return false;
  }
  for (;;) {
    if (leavesTheFunction(child)) return true;
    const parent: Node | undefined = child.getParent();
    if (!parent) return true;
    if (isStatementList(parent)) {
      const statements = parent.getStatements();
      for (const later of statements.slice(statements.indexOf(child) + 1)) {
        if (acts.some((act) => inside(act, later))) return false;
        if (leavesTheFunction(later)) return true;
      }
    }
    if (isLoop(parent)) return false;
    if (Node.isTryStatement(parent)) {
      const closing = parent.getFinallyBlock();
      if (closing && closing !== child && acts.some((act) => inside(act, closing))) return false;
    }
    if (parent === handler || isHandlerFunction(parent)) return true;
    child = parent;
  }
}

/** True when `node` sits in a catch clause of `handler`'s own body. */
export function inCatchClause(node: Node, handler: HandlerFunction): boolean {
  const found = node.getFirstAncestor(
    (ancestor) => ancestor === handler || Node.isCatchClause(ancestor)
  );
  return found !== undefined && Node.isCatchClause(found);
}

/**
 * The statements that run before `node` on its path, nearest first: the ones
 * before it in its own list, then the ones before the statement that holds
 * that list, out to the handler's body. A statement an earlier `if` holds is
 * not on the list; the `if` itself is.
 */
export function statementsBefore(node: Node, handler: HandlerFunction): Node[] {
  const before: Node[] = [];
  let child = statementHolding(node, handler);
  while (child) {
    const parent: Node | undefined = child.getParent();
    if (!parent) break;
    if (isStatementList(parent)) {
      const statements = parent.getStatements();
      before.push(...statements.slice(0, statements.indexOf(child)).reverse());
    }
    if (parent === handler || isHandlerFunction(parent)) break;
    child = parent;
  }
  return before;
}
