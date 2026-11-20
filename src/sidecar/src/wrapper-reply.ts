/**
 * carrick#1969 T1: what a wrapper call does with the callback it is handed,
 * read from the wrapper's own code.
 *
 * A route handler is often a name bound to a wrapper call
 * (`const list = wrap(async (req, res) => …)`). Reading the callback as the
 * route's handler is right only where the wrapper sends what the callback
 * sends. A wrapper that builds the reply (`res.json({ data: await fn(req) })`)
 * puts something else on the wire, so the callback's bare return is not the
 * route's response. The verdict, in this order:
 *
 *  1. The wrapper returns the callback itself: `callback`.
 *  2. The wrapper has no body in the program (a library's `.d.ts`): a
 *     callback declared to return only nothing (`void`, `undefined`,
 *     `Promise<void>`) is `callback`; any other is `unread`.
 *  3. Every use of the wrapper's callback parameter keeps the callback's
 *     result as it is: a call whose value is dropped or returned (through
 *     `await`, `void`, parentheses, casts, `!`, a promise's `catch` /
 *     `finally` and `Promise.resolve`, each decided by its declaration in the
 *     default library); a call whose value is the whole first argument of a
 *     send on a parameter of the function the wrapper returns, directly, through
 *     `.then((x) => res.send(x))` or through a `const` it is bound to; the
 *     callback handed to a call whose own result is a primitive (an
 *     inspection) or to another wrapper call that is itself `callback`; a
 *     thunk of the callback handed to a helper with a body that sends the
 *     thunk's result unchanged on a reply it was handed (one hop, else the
 *     whole wrapper is `unread`). Then `callback`.
 *  4. Anything else: `runs` the one function literal the wrapper returns,
 *     whose reply is the route's, else `unread`.
 *
 * Where labels name the service's send members, a send must be a labelled
 * one whose payload is its first argument. No method or framework name is
 * consulted beyond the default library's promise members.
 */

import {
  Node,
  SyntaxKind,
  type ArrowFunction,
  type CallExpression,
  type FunctionExpression,
  type Identifier,
  type Symbol as TsSymbol,
  type Type,
} from 'ts-morph';
import { functionBodyOf, peelValueWrappers, type PlainFunctionBody } from './function-body.js';
import { defaultLibraryMember } from './retype-selector.js';
import type { ClaimSlot } from './types.js';

type FunctionLiteral = ArrowFunction | FunctionExpression;

/** What the route's reply is read from, for a handler bound to a wrapper call. */
export type ReplyVerdict =
  | { kind: 'callback' }
  | { kind: 'runs'; handler: FunctionLiteral }
  | { kind: 'unread' };

export const CALLBACK_VERDICT: ReplyVerdict = { kind: 'callback' };
const UNREAD: ReplyVerdict = { kind: 'unread' };

/** How deep a wrapper's hand-offs to other wrappers are followed. */
const MAX_DEPTH = 3;

export interface WrapperReplyContext {
  /** Whether labels name any send member; then a send must be a labelled one. */
  readonly readsReplies: boolean;
  /** The labels' word on a call (`StatedParams.sendAt`). */
  sendAt(call: Node): ClaimSlot | 'setter' | undefined;
  /** The symbol at the root of a member or call chain (`StatedParams.rootSymbol`). */
  rootSymbol(expression: Node): TsSymbol | undefined;
  /** A type that holds no value (`StatedParams.isNothing`). */
  isNothing(type: Type): boolean;
  /** A promise's value type, unchanged for any other type. */
  awaited(type: Type): Type;
}

/**
 * Nested wrappers (`outer(inner(fn))`) compose innermost first: any `unread`
 * level makes the whole `unread`, else the innermost level that is not
 * `callback` decides.
 */
export function composeReplyVerdicts(inner: ReplyVerdict, outer: ReplyVerdict): ReplyVerdict {
  if (inner.kind === 'unread' || outer.kind === 'unread') return UNREAD;
  return inner.kind === 'runs' ? inner : outer;
}

/** The verdict for the callback handed to `call` as argument `index`. */
export function wrapperReplyVerdict(
  call: CallExpression,
  index: number,
  context: WrapperReplyContext,
  depth = 0
): ReplyVerdict {
  if (depth >= MAX_DEPTH) return UNREAD;
  const wrapper = bodyOfCallee(call);
  if (!wrapper) return declaredCallbackVerdict(call, index, context);
  const parameter = parameterSymbol(wrapper, index);
  if (!parameter) return UNREAD;
  if (returnsOnly(wrapper, parameter.symbol)) return CALLBACK_VERDICT;
  let keeps = true;
  for (const use of usesOf(wrapper, parameter.symbol, parameter.name)) {
    const kept = useKeepsResult(use, wrapper, context, depth);
    if (kept === 'unread') return UNREAD;
    if (!kept) keeps = false;
  }
  if (keeps) return CALLBACK_VERDICT;
  const replier = onlyReturnedFunction(wrapper);
  return replier ? { kind: 'runs', handler: replier } : UNREAD;
}

/** The one body the callee declares in the program, or undefined (a library, several bodies). */
function bodyOfCallee(call: CallExpression): PlainFunctionBody | undefined {
  let symbol = call.getExpression().getSymbol();
  if (symbol?.isAlias()) symbol = symbol.getAliasedSymbol() ?? symbol;
  const body = functionBodyOf(symbol?.getDeclarations() ?? [], { methods: false });
  return body && !body.getSourceFile().isDeclarationFile() ? body : undefined;
}

function parameterSymbol(
  fn: PlainFunctionBody,
  index: number
): { name: Identifier; symbol: TsSymbol } | undefined {
  const parameter = fn.getParameters()[index];
  if (!parameter || parameter.isRestParameter()) return undefined;
  const name = parameter.getNameNode();
  if (!Node.isIdentifier(name)) return undefined;
  const symbol = name.getSymbol();
  return symbol ? { name, symbol } : undefined;
}

/** Rule 2: a wrapper with no body, judged by the callback type it declares. */
function declaredCallbackVerdict(call: CallExpression, index: number, context: WrapperReplyContext): ReplyVerdict {
  let returns: Type[];
  try {
    const parameter = call.getProject().getTypeChecker().getResolvedSignature(call)?.getParameters()[index];
    if (!parameter) return UNREAD;
    returns = parameter
      .getTypeAtLocation(call)
      .getCallSignatures()
      .map((signature) => signature.getReturnType());
  } catch {
    return UNREAD;
  }
  const parts = returns.flatMap(membersOf).flatMap((type) => membersOf(context.awaited(type)));
  return parts.length > 0 && parts.every((type) => context.isNothing(type)) ? CALLBACK_VERDICT : UNREAD;
}

function membersOf(type: Type): Type[] {
  return type.isUnion() ? type.getUnionTypes() : [type];
}

function isFunctionNode(node: Node): boolean {
  return (
    Node.isArrowFunction(node) ||
    Node.isFunctionExpression(node) ||
    Node.isFunctionDeclaration(node) ||
    Node.isMethodDeclaration(node) ||
    Node.isConstructorDeclaration(node) ||
    Node.isGetAccessorDeclaration(node) ||
    Node.isSetAccessorDeclaration(node)
  );
}

function enclosingFunction(node: Node): Node | undefined {
  return node.getFirstAncestor(isFunctionNode);
}

/** The expressions `fn` itself returns; `undefined` stands for a bare `return;`. */
function ownReturns(fn: PlainFunctionBody): Array<Node | undefined> {
  const body = fn.getBody();
  if (!body) return [];
  if (!Node.isBlock(body)) return [body];
  return body
    .getDescendantsOfKind(SyntaxKind.ReturnStatement)
    .filter((statement) => enclosingFunction(statement) === fn)
    .map((statement) => statement.getExpression());
}

function returnsOnly(fn: PlainFunctionBody, symbol: TsSymbol): boolean {
  const returned = ownReturns(fn);
  return (
    returned.length > 0 &&
    returned.every((expression) => {
      const value = expression ? peelValueWrappers(expression) : undefined;
      return !!value && Node.isIdentifier(value) && value.getSymbol() === symbol;
    })
  );
}

/** The one function literal `fn` returns on every path that returns, else undefined. */
function onlyReturnedFunction(fn: PlainFunctionBody): FunctionLiteral | undefined {
  const returned = ownReturns(fn);
  if (returned.length !== 1 || !returned[0]) return undefined;
  const value = peelValueWrappers(returned[0]);
  return Node.isArrowFunction(value) || Node.isFunctionExpression(value) ? value : undefined;
}

/** Every reference to `symbol` inside `scope`, the declaring name left out. */
function usesOf(scope: Node, symbol: TsSymbol, declared: Node): Identifier[] {
  const checker = scope.getProject().getTypeChecker();
  return scope.getDescendantsOfKind(SyntaxKind.Identifier).filter((id) => {
    if (id === declared) return false;
    const parent = id.getParent();
    if (parent && Node.isShorthandPropertyAssignment(parent)) {
      return checker.getShorthandAssignmentValueSymbol(parent) === symbol;
    }
    return id.getSymbol() === symbol;
  });
}

/**
 * Climb from a call through what leaves its value as it is: `await`,
 * parentheses, casts, `!`, the default library's `catch` / `finally` on it and
 * `Promise.resolve(it)`.
 */
function climbKeptValue(node: Node): Node {
  let top = node;
  for (;;) {
    const parent = top.getParent();
    if (!parent) return top;
    if (
      (Node.isParenthesizedExpression(parent) ||
        Node.isAwaitExpression(parent) ||
        Node.isAsExpression(parent) ||
        Node.isSatisfiesExpression(parent) ||
        Node.isNonNullExpression(parent) ||
        Node.isTypeAssertion(parent)) &&
      parent.getExpression() === top
    ) {
      top = parent;
      continue;
    }
    if (Node.isPropertyAccessExpression(parent) && parent.getExpression() === top) {
      const member = parent.getParent();
      const name = member && Node.isCallExpression(member) && member.getExpression() === parent
        ? defaultLibraryMember(member)
        : undefined;
      if (member && (name === 'catch' || name === 'finally')) {
        top = member;
        continue;
      }
      return top;
    }
    if (
      Node.isCallExpression(parent) &&
      parent.getArguments().length === 1 &&
      parent.getArguments()[0] === top &&
      defaultLibraryMember(parent) === 'resolve'
    ) {
      top = parent;
      continue;
    }
    return top;
  }
}

/** Whether `symbol` is a parameter of a function that holds `at` and sits inside `outer`. */
function parameterInside(symbol: TsSymbol | undefined, at: Node, outer: Node | undefined): boolean {
  if (!symbol) return false;
  return symbol.getDeclarations().some((declaration) => {
    if (!Node.isParameterDeclaration(declaration)) return false;
    const owner = declaration.getParent();
    if (owner === outer) return false;
    const holdsAt = at.getAncestors().includes(owner);
    return holdsAt && (outer === undefined || owner.getAncestors().includes(outer));
  });
}

type RootTest = (root: TsSymbol | undefined, send: CallExpression) => boolean;

/** Whether `send` is a send of its whole first argument on a receiver `rootOk` accepts. */
function isSend(send: CallExpression, rootOk: RootTest, context: WrapperReplyContext): boolean {
  const callee = send.getExpression();
  if (!Node.isPropertyAccessExpression(callee)) return false;
  if (!rootOk(context.rootSymbol(callee.getExpression()), send)) return false;
  if (!context.readsReplies) return true;
  const slot = context.sendAt(send);
  return typeof slot === 'object' && slot.arg === 0 && slot.key === undefined;
}

/** A read that only tests a value (`x !== undefined`, `!x`, `typeof x`, `if (x)`). */
function isInspection(use: Node): boolean {
  const parent = use.getParent();
  if (!parent) return false;
  if (Node.isBinaryExpression(parent)) {
    const operator = parent.getOperatorToken().getKind();
    return (
      operator === SyntaxKind.EqualsEqualsEqualsToken ||
      operator === SyntaxKind.ExclamationEqualsEqualsToken ||
      operator === SyntaxKind.EqualsEqualsToken ||
      operator === SyntaxKind.ExclamationEqualsToken
    );
  }
  if (Node.isPrefixUnaryExpression(parent)) return parent.getOperatorToken() === SyntaxKind.ExclamationToken;
  if (Node.isTypeOfExpression(parent)) return true;
  if (Node.isIfStatement(parent)) return parent.getExpression() === use;
  return false;
}

/**
 * Whether `value` is sent unchanged: the whole first argument of a send,
 * handed on by `.then((x) => send(x))`, or bound to a `const` that is only
 * sent so and tested.
 */
function sentUnchanged(value: Node, rootOk: RootTest, context: WrapperReplyContext): boolean {
  const parent = value.getParent();
  if (!parent) return false;
  if (Node.isCallExpression(parent) && parent.getExpression() !== value) {
    return parent.getArguments()[0] === value && isSend(parent, rootOk, context);
  }
  if (Node.isPropertyAccessExpression(parent) && parent.getExpression() === value && parent.getName() === 'then') {
    const then = parent.getParent();
    if (!then || !Node.isCallExpression(then) || then.getExpression() !== parent || then.getArguments().length !== 1) {
      return false;
    }
    const next = peelValueWrappers(then.getArguments()[0]);
    if (!Node.isArrowFunction(next) && !Node.isFunctionExpression(next)) return false;
    const handed = parameterSymbol(next, 0);
    if (!handed || next.getParameters().length !== 1) return false;
    const uses = usesOf(next, handed.symbol, handed.name);
    if (uses.length === 0 || !uses.every((use) => sentUnchanged(use, rootOk, context))) return false;
    const after = climbKeptValue(then).getParent();
    return !!after && (Node.isExpressionStatement(after) || Node.isVoidExpression(after) || Node.isReturnStatement(after));
  }
  if (Node.isVariableDeclaration(parent) && parent.getInitializer() === value) {
    const name = parent.getNameNode();
    const list = parent.getParent();
    if (!Node.isIdentifier(name) || !Node.isVariableDeclarationList(list) || list.getDeclarationKind() !== 'const') {
      return false;
    }
    const symbol = name.getSymbol();
    const scope = enclosingFunction(parent);
    if (!symbol || !scope) return false;
    let sent = false;
    for (const use of usesOf(scope, symbol, name)) {
      if (sentUnchanged(use, rootOk, context)) sent = true;
      else if (!isInspection(use)) return false;
    }
    return sent;
  }
  return false;
}

/**
 * Rule 3 for one reference to the wrapper's callback parameter: `true` when
 * it keeps the callback's result as it is, `false` when it does something
 * else with it, `'unread'` when it hands a thunk of the callback to a helper
 * that does not send the thunk's result unchanged.
 */
function useKeepsResult(
  use: Identifier,
  wrapper: PlainFunctionBody,
  context: WrapperReplyContext,
  depth: number
): boolean | 'unread' {
  const parent = use.getParent();
  if (parent && Node.isCallExpression(parent) && parent.getExpression() === use) {
    return resultKept(parent, wrapper, context, depth);
  }
  const handedTo = handedAsArgument(use);
  if (!handedTo) return false;
  if (callResultIsPrimitive(handedTo.call)) return true;
  return wrapperReplyVerdict(handedTo.call, handedTo.index, context, depth + 1).kind === 'callback';
}

function handedAsArgument(node: Node): { call: CallExpression; index: number } | undefined {
  let top = node;
  let parent = top.getParent();
  while (parent && peelValueWrappers(parent) === top && parent !== top) {
    top = parent;
    parent = top.getParent();
  }
  if (!parent || !Node.isCallExpression(parent) || parent.getExpression() === top) return undefined;
  const index = parent.getArguments().indexOf(top);
  return index >= 0 ? { call: parent, index } : undefined;
}

/** Whether a call's own result is a primitive: a value, never a registration or a wrapper. */
export function callResultIsPrimitive(call: CallExpression): boolean {
  let own: Type;
  try {
    own = call.getType();
  } catch {
    return false;
  }
  return (
    own.isString() ||
    own.isNumber() ||
    own.isBoolean() ||
    own.isStringLiteral() ||
    own.isNumberLiteral() ||
    own.isBooleanLiteral()
  );
}

function resultKept(
  callbackCall: CallExpression,
  wrapper: PlainFunctionBody,
  context: WrapperReplyContext,
  depth: number
): boolean | 'unread' {
  const top = climbKeptValue(callbackCall);
  const parent = top.getParent();
  if (!parent) return false;
  if (Node.isExpressionStatement(parent) || Node.isVoidExpression(parent)) return true;
  const returned =
    Node.isReturnStatement(parent) || (Node.isArrowFunction(parent) && parent.getBody() === top);
  if (returned) {
    const from = enclosingFunction(top);
    if (!from || from === wrapper) return !!from;
    if (ownReturns(wrapper).some((expression) => expression && peelValueWrappers(expression) === from)) return true;
    if (!Node.isArrowFunction(from) && !Node.isFunctionExpression(from)) return false;
    // The callback's result is the result of a function handed to a call.
    const handedTo = handedAsArgument(from);
    if (!handedTo) return false;
    if (wrapperReplyVerdict(handedTo.call, handedTo.index, context, depth + 1).kind === 'callback') return true;
    return from.getParameters().length === 0 && thunkSentUnchanged(handedTo.call, handedTo.index, context)
      ? true
      : 'unread';
  }
  return sentUnchanged(top, (root, send) => parameterInside(root, send, wrapper), context);
}

/**
 * A2, one hop: a helper with a body whose every use of the thunk parameter is
 * a call, with no arguments, whose value is sent unchanged on a parameter of
 * the helper that the call site hands a parameter of an enclosing function
 * (the route's own reply).
 */
function thunkSentUnchanged(helperCall: CallExpression, index: number, context: WrapperReplyContext): boolean {
  const helper = bodyOfCallee(helperCall);
  if (!helper) return false;
  const thunk = parameterSymbol(helper, index);
  if (!thunk) return false;
  const replies = new Set<TsSymbol>();
  helper.getParameters().forEach((parameter, position) => {
    const argument = helperCall.getArguments()[position];
    const value = argument ? peelValueWrappers(argument) : undefined;
    const symbol = parameter.getNameNode().getSymbol();
    if (symbol && value && Node.isIdentifier(value) && parameterInside(value.getSymbol(), helperCall, undefined)) {
      replies.add(symbol);
    }
  });
  const uses = usesOf(helper, thunk.symbol, thunk.name);
  return (
    uses.length > 0 &&
    uses.every((use) => {
      const parent = use.getParent();
      if (!parent || !Node.isCallExpression(parent) || parent.getExpression() !== use) return false;
      if (parent.getArguments().length !== 0) return false;
      return sentUnchanged(climbKeptValue(parent), (root) => !!root && replies.has(root), context);
    })
  );
}
