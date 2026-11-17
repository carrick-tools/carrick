/**
 * What a reply's HTTP status says about its body (carrick#1161), read off the
 * compiler's own API so every path that holds a located reply decides it the
 * same way.
 *
 * `success` (below 300) and `undecided` (no status stated anywhere) send the
 * route's contract; `error` (400 and up) and `redirect` (3xx) do not; and
 * `variable` is a status the source computes, which fails open into the union
 * so no success body is ever guessed away.
 *
 * Inputs are a checker and a node. Nothing here consults a method, a
 * framework or a library name: the status is read from an argument past the
 * body, from an earlier call of the same chain, or from the call's result
 * type. Whether a call IS a reply stays with each caller.
 */

import ts from 'typescript';
import { classifyStatusCodes, httpStatus } from './status-table.js';

export type ReplyStatus = 'success' | 'error' | 'redirect' | 'variable' | 'undecided';

/**
 * Members an options object uses to state an HTTP status. A >= 400 status marks
 * the branch an error path, whose shape is not the endpoint's contract.
 */
export const STATUS_MEMBER_NAMES = ['status', 'statusCode'] as const;

/** An error or redirect reply carries no body the route's contract describes. */
export function isDroppedReplyStatus(status: ReplyStatus): boolean {
  return status === 'error' || status === 'redirect';
}

/**
 * The status codes a TYPE fixes: a numeric literal, or a union made only of
 * them, every one in the HTTP range. `undefined` for anything else, including
 * a plain `number`.
 */
export function statusCodesOfType(type: ts.Type): number[] | undefined {
  const members = type.flags & ts.TypeFlags.Union ? (type as ts.UnionType).types : [type];
  const codes: number[] = [];
  for (const member of members) {
    if (!(member.flags & ts.TypeFlags.NumberLiteral)) return undefined;
    const code = httpStatus((member as ts.NumberLiteralType).value);
    if (code === undefined) return undefined;
    codes.push(code);
  }
  return codes.length > 0 ? codes : undefined;
}

/** Peel parentheses and `await`; `as` and `satisfies` are kept. */
function peel(node: ts.Node): ts.Node {
  let current = node;
  while (ts.isParenthesizedExpression(current) || ts.isAwaitExpression(current)) {
    current = current.expression;
  }
  return current;
}

/** The value of the first object-literal member `name` written as a plain identifier. */
function propertyNamed(
  literal: ts.ObjectLiteralExpression,
  name: string
): ts.ObjectLiteralElementLike | undefined {
  return literal.properties.find(
    (property) =>
      property.name !== undefined && ts.isIdentifier(property.name) && property.name.text === name
  );
}

/**
 * The HTTP status codes an argument states, `'variable'` when it is a
 * status-shaped value the source does not fix, or `undefined` when it says
 * nothing about a status.
 *
 * Read from the AST first: `{ status: 400 }` in an argument position widens
 * to `{ status: number }`, so the literal only survives syntactically. The
 * type read behind it catches `as const`, hoisted option objects and
 * literal-typed variables. Only values in the HTTP range count: an
 * arbitrary number named `status` on a domain object (`{ status: 2 }`)
 * states nothing about transport.
 */
export function statedStatusCodes(
  checker: ts.TypeChecker,
  node: ts.Node
): number[] | 'variable' | undefined {
  if (ts.isNumericLiteral(node)) {
    const code = httpStatus(parseFloat(node.text));
    return code === undefined ? undefined : [code];
  }

  if (ts.isObjectLiteralExpression(node)) {
    for (const name of STATUS_MEMBER_NAMES) {
      const property = propertyNamed(node, name);
      if (!property || !ts.isPropertyAssignment(property)) continue;
      const read = statedStatusCodes(checker, peel(property.initializer));
      if (read) return read;
    }
    return undefined;
  }

  const type = checker.getTypeAtLocation(node);
  const fromType = statusCodesOfType(type);
  if (fromType) return fromType;
  if (type.flags & ts.TypeFlags.Number) return 'variable';
  for (const name of STATUS_MEMBER_NAMES) {
    const property = checker.getPropertyOfType(type, name);
    const declaration = property?.declarations?.[0];
    if (!property || !declaration) continue;
    const propertyType = checker.getNonNullableType(
      checker.getTypeOfSymbolAtLocation(property, declaration)
    );
    const codes = statusCodesOfType(propertyType);
    if (codes) return codes;
    if (propertyType.flags & ts.TypeFlags.Number) return 'variable';
  }
  return undefined;
}

/** The status a value states, classified as a send's status argument is. */
export function valueStatus(checker: ts.TypeChecker, value: ts.Node | undefined): ReplyStatus {
  if (!value) return 'undecided';
  const read = statedStatusCodes(checker, peel(value));
  if (read === 'variable') return 'variable';
  if (!read) return 'undecided';
  const kind = classifyStatusCodes(read);
  return kind === 'mixed' ? 'variable' : kind;
}

/**
 * The status an earlier call of a send's own chain states: the receiver
 * calls `call` is invoked on (`a.status(401).json(body)` has `a.status(401)`),
 * innermost last. Each receiver call's first argument is read by the rule
 * an argument past a body is, so a numeric literal or a literal-typed value
 * counts and a plain `number` is `variable`. `undefined` when no receiver
 * call states a status. No method name is consulted: a receiver call that
 * is handed no status-shaped value says nothing.
 */
export function chainStatus(checker: ts.TypeChecker, call: ts.Node): ReplyStatus | undefined {
  if (!ts.isCallExpression(call)) return undefined;
  let callee: ts.Expression = call.expression;
  while (ts.isPropertyAccessExpression(callee)) {
    const receiver = peel(callee.expression);
    if (!ts.isCallExpression(receiver)) return undefined;
    const status = valueStatus(checker, receiver.arguments[0]);
    if (status !== 'undecided') return status;
    callee = receiver.expression;
  }
  return undefined;
}

/**
 * `Promise<X>` / `PromiseLike<X>` / the surfaced `Awaited<X>` alias, peeled to
 * `X` by name. Any other type is returned unchanged.
 */
function unwrapPromise(checker: ts.TypeChecker, type: ts.Type): ts.Type {
  // Bounded recursion guard: a pathological self-referential alias must not
  // loop. Real promise nesting is shallow; 16 is far more than enough.
  let current = type;
  for (let depth = 0; depth < 16; depth++) {
    const symbolName = current.getSymbol()?.getName();
    const aliasName = current.aliasSymbol?.getName();

    if (symbolName === 'Promise' || symbolName === 'PromiseLike') {
      const args =
        (current as ts.ObjectType).objectFlags & ts.ObjectFlags.Reference
          ? checker.getTypeArguments(current as ts.TypeReference)
          : [];
      if (args.length === 1) {
        current = args[0];
        continue;
      }
      return current;
    }

    if (aliasName === 'Awaited') {
      const aliasArgs = current.aliasTypeArguments ?? [];
      if (aliasArgs.length === 1) {
        current = aliasArgs[0];
        continue;
      }
      return current;
    }

    return current;
  }
  return current;
}

/**
 * Status codes a send call's result type records: the members (of the result,
 * or of each part of an intersection result) typed as a status literal or a
 * union of them. `undefined` when none is.
 */
export function resultTypeStatusCodes(
  checker: ts.TypeChecker,
  call: ts.Node
): number[] | undefined {
  const result = unwrapPromise(checker, checker.getTypeAtLocation(call));
  const parts = result.flags & ts.TypeFlags.Intersection ? (result as ts.IntersectionType).types : [result];
  const codes: number[] = [];
  for (const part of parts) {
    if (part.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown) || !(part.flags & ts.TypeFlags.Object)) {
      continue;
    }
    for (const property of checker.getPropertiesOfType(part)) {
      const codesHere = statusCodesOfType(checker.getTypeOfSymbolAtLocation(property, call));
      if (codesHere) codes.push(...codesHere);
    }
  }
  return codes.length > 0 ? codes : undefined;
}

/**
 * What a reply states about its HTTP status (carrick#1161).
 *
 * Read in order, and the first that states anything decides:
 *
 *  1. an argument past the body that carries a status: a numeric literal
 *     (`send(body, 404)`), an options object (`{ status: 401 }`), or any
 *     expression whose TYPE is a status literal or a union of them
 *     (`let code: 400 | 500`). A numeric argument whose type is a plain
 *     `number` states a status the source does not fix, which is
 *     `variable`: the branch fails open into the union and is logged.
 *  2. the call's own chain, when `readChain` is set (carrick#2005):
 *     `reply.status(401).json(body)`.
 *  3. the call's own RESULT type, when a member of it is typed as a status
 *     literal. A framework that types its sends records the status there, and
 *     a redirect helper's default `302` is only visible there. A result whose
 *     status member spans success and error codes (the default of a typed
 *     `json(body, status?)`) says nothing either way.
 *
 * A status in the FIRST argument is a field of the body and is never read.
 * No method or framework name is consulted anywhere.
 */
export function replyStatus(
  checker: ts.TypeChecker,
  expression: ts.Node,
  opts: { readChain: boolean }
): ReplyStatus {
  const call = peel(expression);
  if (!ts.isCallExpression(call) && !ts.isNewExpression(call)) {
    return 'undecided';
  }
  const args = (call.arguments ?? []).map((a) => peel(a));
  const stated: number[] = [];
  let variable = false;
  for (const arg of args.slice(1)) {
    const read = statedStatusCodes(checker, arg);
    if (read === 'variable') {
      variable = true;
    } else if (read) {
      stated.push(...read);
    }
  }
  if (stated.length > 0) {
    const kind = classifyStatusCodes(stated);
    return kind === 'mixed' ? 'variable' : kind;
  }
  if (variable) return 'variable';

  // carrick#2005: a status an earlier call of the same chain states. Read only
  // where the caller asks for it; the return walk does not read it yet
  // (carrick#1962).
  if (opts.readChain) {
    const chained = chainStatus(checker, call);
    if (chained) return chained;
  }

  const typed = resultTypeStatusCodes(checker, call);
  if (typed) {
    const kind = classifyStatusCodes(typed);
    if (kind !== 'mixed') return kind;
  }
  return 'undecided';
}
