/**
 * What a route handler's own parameters state about the route's bodies
 * (carrick#2674), read through the members the service's verified
 * server-framework labels name ({@link LabelledMembers}). A member is matched
 * by its declaration, never by its name, so a type the repo declares itself
 * with a member of the same name states nothing here.
 *
 * - The request: the member a receive op's `request.body` path ends on, read
 *   at the handler's parameter. A type the source fills is the body. A body
 *   the source leaves `never`, or leaves at the library's unfilled `unknown`
 *   while it writes a type argument for the route, is "no request body",
 *   provided nothing reads it (`bodyIsUnread`).
 * - The reply: the payload a labelled `send` member's slot takes, read at the
 *   handler's parameter, where the library declares that payload through a
 *   type parameter of the reply type. A union of success and error bodies is
 *   cut to the variants a success-status send through the parameter carries.
 */

import {
  Node,
  SyntaxKind,
  ts,
  type ArrowFunction,
  type BinaryExpression,
  type CallExpression,
  type FunctionDeclaration,
  type FunctionExpression,
  type MethodDeclaration,
  type ParameterDeclaration,
  type PropertyAccessExpression,
  type Symbol as TsSymbol,
  type Type,
} from 'ts-morph';
import { declarationKey, type LabelledMembers } from './library-claims.js';
import type { ClaimSlot, ResultShape } from './types.js';

/** A function a handler can be: what the inferrer follows a registration to. */
export type HandlerFunction = FunctionDeclaration | ArrowFunction | FunctionExpression | MethodDeclaration;

/** What a handler's parameters state about the request body. */
export type StatedRequest =
  | { kind: 'body'; type: Type; at: ParameterDeclaration }
  | { kind: 'no_body'; at: ParameterDeclaration; detail: string };

/** The reply a handler's parameters state: one or more success variants. */
export interface StatedReply {
  variants: Type[];
  at: Node;
}

/** The status a send's own chain states, as the inferrer reads it. */
export type ChainStatus = (call: CallExpression) => 'success' | 'error' | 'redirect' | 'variable' | 'undecided' | undefined;

/** The status a value states, read as a send's status argument is: `undecided` when it states none. */
export type ValueStatus = (value: Node) => 'success' | 'error' | 'redirect' | 'variable' | 'undecided';

/** The node a value is read from: `JSON.stringify(x)` is read as `x`. */
export type PayloadNode = (value: Node) => Node;

/** Type flags of a value that cannot hold a request body: a string, number, boolean and the like. */
const PLAIN_VALUE_FLAGS =
  ts.TypeFlags.StringLike |
  ts.TypeFlags.NumberLike |
  ts.TypeFlags.BigIntLike |
  ts.TypeFlags.BooleanLike |
  ts.TypeFlags.EnumLike |
  ts.TypeFlags.Undefined |
  ts.TypeFlags.Null |
  ts.TypeFlags.Void;

/** The expression's type is a plain value, or a union of plain values. */
function holdsPlainValue(node: Node): boolean {
  let type: Type;
  try {
    type = node.getType();
  } catch {
    return false;
  }
  const parts = type.isUnion() ? type.getUnionTypes() : [type];
  return parts.every((part) => (part.getFlags() & PLAIN_VALUE_FLAGS) !== 0);
}

/** How many repo functions the request may be handed through before a read is unprovable. */
const REACH_DEPTH = 2;

export class StatedParams {
  constructor(
    private readonly labels: LabelledMembers,
    /** True for a file the repo declares itself (not a library's or the runtime's). */
    private readonly isRepoFile: (file: Node) => boolean,
    private readonly chainStatus: ChainStatus,
    private readonly valueStatus: ValueStatus,
    private readonly payloadNode: PayloadNode
  ) {}

  /** No label names a member: every reading answers nothing. */
  get isEmpty(): boolean {
    return (
      this.labels.sends.size === 0 &&
      this.labels.sets.size === 0 &&
      this.labels.bodies.size === 0 &&
      this.labels.returns.size === 0
    );
  }

  /** Whether some label names a reply's send member, called or set by assignment. */
  get readsReplies(): boolean {
    return this.labels.sends.size > 0 || this.labels.sets.size > 0;
  }

  /** Whether some label says a route's handler returns the reply (carrick#3054). */
  get readsReturns(): boolean {
    return this.labels.returns.size > 0;
  }

  // ===========================================================================
  // Request
  // ===========================================================================

  /**
   * The request body the handler's parameters state, the first parameter
   * whose type has a labelled body member deciding. Undefined when no
   * parameter has one, or the one that does states nothing decidable. A
   * route whose `registration` hands in another function beside the handler
   * (a middleware, which can read the body first) decides no "no body".
   */
  request(handler: HandlerFunction, registration?: Node): StatedRequest | undefined {
    if (this.labels.bodies.size === 0) return undefined;
    const parameters = handler.getParameters();
    for (const parameter of parameters) {
      const member = this.labelledMember(parameter, (key) => this.labels.bodies.has(key));
      if (!member) continue;
      let body: Type;
      try {
        body = member.getTypeAtLocation(parameter);
      } catch {
        return undefined;
      }
      if (body.isAny()) return undefined;
      if (!body.isUnknown() && !body.isNever()) return { kind: 'body', type: body, at: parameter };
      if (body.isUnknown() && !this.writesTypeArgument(handler, parameter)) return undefined;
      if (registration && this.handsInAnotherFunction(registration, handler)) return undefined;
      const declarations = new Set((member.getDeclarations() ?? []).map((d) => declarationKey(d.compilerNode)));
      if (!this.bodyIsUnread(handler, parameters.indexOf(parameter), declarations, 0, new Map())) {
        return undefined;
      }
      return {
        kind: 'no_body',
        at: parameter,
        detail: body.isNever()
          ? "the route's request type states its body as never, and nothing the handler hands the request to reads it"
          : "the route's request type states its parts and no body, and nothing the handler hands the request to reads it",
      };
    }
    return undefined;
  }

  /**
   * The route hands in a function besides the handler: a callable argument
   * (or list of them) beside the handler on any call it is passed through
   * up to the registration, or, where the registration names the handler by
   * reference, a second callable argument or property there. What such a
   * function (a middleware) does with the request is not read here.
   */
  handsInAnotherFunction(registration: Node, handler: HandlerFunction): boolean {
    const callable = (node: Node): boolean => {
      let type: Type;
      try {
        type = node.getType();
      } catch {
        return true;
      }
      const element = type.getArrayElementType() ?? type.getTupleElements()[0];
      return type.getCallSignatures().length > 0 || (element !== undefined && element.getCallSignatures().length > 0);
    };
    let node: Node = handler;
    let parent = node.getParent();
    while (parent && Node.isCallExpression(parent) && parent.getArguments().includes(node)) {
      const passed = node;
      if (parent.getArguments().some((argument) => argument !== passed && callable(argument))) return true;
      node = parent;
      parent = node.getParent();
    }
    const holdsHandler = registration === handler || handler.getAncestors().includes(registration);
    if (holdsHandler) {
      if (!Node.isObjectLiteralExpression(registration)) return false;
      return registration
        .getProperties()
        .some((property) => !handler.getAncestors().includes(property) && callable(property));
    }
    const siblings: Node[] = Node.isCallExpression(registration)
      ? registration.getArguments()
      : Node.isObjectLiteralExpression(registration)
        ? registration.getProperties()
        : [];
    return siblings.filter(callable).length > 1;
  }

  /**
   * A type argument the source writes for this route: on a call the handler
   * is passed to (the registration, or a wrapper that makes the handler),
   * through any calls enclosing that one as an argument, or on the
   * parameter's own annotation.
   */
  private writesTypeArgument(handler: HandlerFunction, parameter: ParameterDeclaration): boolean {
    const annotation = parameter.getTypeNode();
    if (annotation && Node.isTypeReference(annotation) && annotation.getTypeArguments().length > 0) {
      return true;
    }
    let node: Node = handler;
    let parent = node.getParent();
    while (parent && Node.isCallExpression(parent) && parent.getArguments().includes(node)) {
      if (parent.getTypeArguments().length > 0) return true;
      node = parent;
      parent = node.getParent();
    }
    return false;
  }

  /**
   * Nothing reads the labelled body member off parameter `index` of
   * `func`, nor off the request in the repo functions it is handed to, up to
   * `REACH_DEPTH` calls deep. Every use of the request must be a member
   * read that is not the body and is not itself called (a framework's own
   * body reader), or a hand-off to a function the repo declares, whole and
   * by position. Anything else (a destructured parameter, a library
   * function, an alias, a spread) is a read this cannot rule out.
   */
  private bodyIsUnread(
    func: HandlerFunction,
    index: number,
    bodyKeys: ReadonlySet<string>,
    depth: number,
    seen: Map<Node, Set<number>>
  ): boolean {
    const visited = seen.get(func) ?? new Set<number>();
    if (visited.has(index)) return true;
    visited.add(index);
    seen.set(func, visited);
    const parameter = func.getParameters()[index];
    if (!parameter) return true;
    if (parameter.isRestParameter()) return false;
    const name = parameter.getNameNode();
    if (!Node.isIdentifier(name)) return false;
    const symbol = name.getSymbol();
    const body = func.getBody();
    if (!symbol || !body) return false;
    for (const use of body.getDescendantsOfKind(SyntaxKind.Identifier)) {
      if (use.getSymbol() !== symbol) continue;
      const parent = use.getParent();
      if (!parent) return false;
      if (Node.isPropertyAccessExpression(parent) && parent.getExpression() === use) {
        const declarations = parent.getNameNode().getSymbol()?.getDeclarations() ?? [];
        if (declarations.some((d) => bodyKeys.has(declarationKey(d.compilerNode)))) return false;
        if (!this.memberReadStaysPut(parent)) return false;
        continue;
      }
      if (Node.isCallExpression(parent) && parent.getArguments().includes(use)) {
        const position = parent.getArguments().indexOf(use);
        if (parent.getArguments().slice(0, position).some(Node.isSpreadElement)) return false;
        if (depth >= REACH_DEPTH) return false;
        const callee = this.repoFunction(parent.getExpression());
        if (!callee) return false;
        // A helper that takes the request under a type without the labelled
        // body member could read the body under another declaration, which
        // nothing here can match.
        const taken = callee.getParameters()[position];
        if (!taken || !this.labelledMember(taken, (key) => bodyKeys.has(key))) return false;
        if (!this.bodyIsUnread(callee, position, bodyKeys, depth + 1, seen)) return false;
        continue;
      }
      return false;
    }
    return true;
  }

  /**
   * A read of a request member (`req.headers`, `req.headers.host`) whose
   * value goes nowhere a body could be read from. Along its member chain, a
   * method called on anything but a plain value (`req.raw.pipe(...)`, or a
   * framework's own body reader `req.file()`) is a read, and so is a
   * non-plain value handed to a function (`getRawBody(req.raw)`). A plain
   * value (a header string) holds no body, whatever is done with it.
   */
  private memberReadStaysPut(read: PropertyAccessExpression): boolean {
    let node: Node = read;
    for (;;) {
      const outer = node.getParent();
      if (!outer) return true;
      if ((Node.isCallExpression(outer) || Node.isNewExpression(outer)) && outer.getExpression() === node) {
        const receiver = Node.isPropertyAccessExpression(node) || Node.isElementAccessExpression(node) ? node.getExpression() : undefined;
        return receiver !== undefined && receiver !== read.getExpression() && holdsPlainValue(receiver);
      }
      if (Node.isCallExpression(outer) || Node.isNewExpression(outer)) {
        return holdsPlainValue(node);
      }
      if (
        (Node.isPropertyAccessExpression(outer) || Node.isElementAccessExpression(outer)) &&
        outer.getExpression() === node
      ) {
        node = outer;
        continue;
      }
      if (Node.isNonNullExpression(outer) || Node.isParenthesizedExpression(outer)) {
        node = outer;
        continue;
      }
      return true;
    }
  }

  /** The function a callee names, where the repo declares it; undefined otherwise. */
  private repoFunction(callee: Node): HandlerFunction | undefined {
    let symbol = callee.getSymbol();
    if (symbol?.isAlias()) symbol = symbol.getAliasedSymbol() ?? symbol;
    for (const declaration of symbol?.getDeclarations() ?? []) {
      if (!this.isRepoFile(declaration.getSourceFile())) continue;
      if (
        Node.isFunctionDeclaration(declaration) ||
        Node.isMethodDeclaration(declaration) ||
        Node.isArrowFunction(declaration) ||
        Node.isFunctionExpression(declaration)
      ) {
        return declaration.getBody() ? declaration : undefined;
      }
      if (Node.isVariableDeclaration(declaration) || Node.isPropertyAssignment(declaration)) {
        const initializer = declaration.getInitializer();
        if (initializer && (Node.isArrowFunction(initializer) || Node.isFunctionExpression(initializer))) {
          return initializer;
        }
      }
    }
    return undefined;
  }

  // ===========================================================================
  // Reply
  // ===========================================================================

  /**
   * The reply the handler's parameters state, cut to its success variants.
   * Undefined when no parameter has a labelled send member that states a
   * payload, when two such members disagree, when the payload is `any`,
   * `unknown` or `never`, or when a union of bodies has no variant a
   * success send through the parameter carries.
   */
  reply(handler: HandlerFunction): StatedReply | undefined {
    if (!this.readsReplies) return undefined;
    for (const parameter of handler.getParameters()) {
      const sends = this.sendsThrough(handler, parameter);
      const sets = this.setsThrough(handler, parameter);
      const stated = this.statedPayload(
        parameter,
        new Set([...sends, ...sets].flatMap((send) => send.keys))
      );
      if (stated === 'none') {
        const assigned = this.assignedPayloads(sets);
        if (assigned === 'abstain') return undefined;
        if (assigned.length > 0) return { variants: assigned, at: parameter };
        continue;
      }
      if (stated === 'abstain') return undefined;
      if (stated.isAny() || stated.isUnknown() || stated.isNever()) return undefined;
      const variants = stated.isUnion() ? stated.getUnionTypes().filter((t) => !t.isNull() && !t.isUndefined()) : [stated];
      const literalUnion = stated.isBoolean() || variants.every((t) => t.isLiteral() || t.isBooleanLiteral());
      if (variants.length <= 1 || literalUnion) {
        // An optional payload slot adds `undefined`: no body sent, not a body.
        const defined = stated.isUnion() ? stated.getUnionTypes().filter((t) => !t.isUndefined()) : [stated];
        return defined.length > 0 ? { variants: defined, at: parameter } : undefined;
      }
      const assigned = this.assignedPayloads(sets);
      const carried = [...this.successPayloads(sends), ...(assigned === 'abstain' ? [] : assigned)];
      const kept = variants.filter((variant) => carried.some((sent) => sent.isAssignableTo(variant)));
      return kept.length > 0 ? { variants: kept, at: parameter } : undefined;
    }
    return undefined;
  }

  /**
   * The reply a route's handler returns (carrick#3054): the registration
   * hands the handler in at the slot a `returns` label names on the route
   * member it calls. The handler's return, awaited, without its `void`,
   * `undefined`, `null` and `never` parts; with `result`, the part of it
   * that label names. Undefined when no label names the route, the handler
   * is not at the slot, nothing is left, a part is `any` or `unknown`, the
   * named member is missing from a part, or the label reads a parse method
   * (not read here). The caller reads a labelled send first: a handler that
   * sends does not also reply by return.
   */
  returned(registration: Node, handler: HandlerFunction): StatedReply | undefined {
    if (this.labels.returns.size === 0 || !Node.isCallExpression(registration)) return undefined;
    const label = this.returnsLabelOf(registration);
    if (!label) return undefined;
    const at = registration.getArguments()[label.handler.arg];
    if (!at || (at !== handler && this.repoFunction(at) !== handler)) return undefined;
    const parts: Type[] = [];
    for (const part of this.settled(handler.getReturnType())) {
      if (this.isNothing(part)) continue;
      parts.push(part);
    }
    let held = parts;
    const shape = label.result;
    if (shape !== undefined) {
      if ('parse' in shape) return undefined;
      if ('member' in shape) {
        held = [];
        for (const part of parts) {
          const member = part.getProperty(shape.member[0]);
          if (!member) return undefined;
          let type: Type;
          try {
            type = member.getTypeAtLocation(handler);
          } catch {
            return undefined;
          }
          for (const each of this.settled(type)) {
            if (!this.isNothing(each)) held.push(each);
          }
        }
      }
    }
    if (held.length === 0 || held.some((part) => part.isAny() || part.isUnknown())) return undefined;
    return { variants: held, at: handler };
  }

  /** The `returns` label of the route member a registration call calls, by its declarations. */
  private returnsLabelOf(call: CallExpression): { handler: ClaimSlot; result?: ResultShape } | undefined {
    const declarations: Node[] = [];
    try {
      const declaration = call.getProject().getTypeChecker().getResolvedSignature(call)?.getDeclaration();
      if (declaration) declarations.push(declaration);
    } catch {
      // The call resolves no signature: the member's own symbol still names it.
    }
    const callee = call.getExpression();
    const named = Node.isPropertyAccessExpression(callee) ? callee.getNameNode() : callee;
    declarations.push(...(named.getSymbol()?.getDeclarations() ?? []));
    for (const declaration of declarations) {
      const label = this.labels.returns.get(declarationKey(declaration.compilerNode));
      if (label) return label;
    }
    return undefined;
  }

  private unions(type: Type): Type[] {
    return type.isUnion() ? type.getUnionTypes() : [type];
  }

  /** The parts of `type`, each read through its promise. */
  private settled(type: Type): Type[] {
    return this.unions(type).flatMap((part) => this.unions(this.awaited(part)));
  }

  /** A promise read through to what it resolves to, at most four hops. */
  private awaited(type: Type): Type {
    let current = type;
    for (let hop = 0; hop < 4; hop++) {
      const name = current.getSymbol()?.getName();
      const inner = current.getTypeArguments()[0];
      if ((name !== 'Promise' && name !== 'PromiseLike') || !inner) break;
      current = inner;
    }
    return current;
  }

  private isNothing(type: Type): boolean {
    return type.isUndefined() || type.isNull() || (type.getFlags() & (ts.TypeFlags.Void | ts.TypeFlags.Never)) !== 0;
  }

  /**
   * The payload the labelled send members the handler calls through the
   * parameter state: `none` when it calls none, `abstain` when they state
   * different payloads, else the one payload type. A member the handler never
   * calls (a reply object's status-code send beside its body send) says
   * nothing about this route.
   */
  private statedPayload(parameter: ParameterDeclaration, called: ReadonlySet<string>): Type | 'none' | 'abstain' {
    let type: Type;
    try {
      type = parameter.getType();
    } catch {
      return 'none';
    }
    let found = false;
    let payload: Type | undefined;
    for (const property of type.getProperties()) {
      const keys = property.getDeclarations().map((d) => declarationKey(d.compilerNode));
      if (!keys.some((key) => called.has(key))) continue;
      const declarations = property.getDeclarations().map((d) => d.compilerNode);
      const assigned = this.setSlotOf(declarations);
      const slot = assigned ?? this.sendSlotOf(declarations);
      if (!slot) continue;
      found = true;
      const through = property
        .getDeclarations()
        .map((d) => this.payloadTypeParameters(d.compilerNode, slot))
        .find((named) => named.length > 0);
      if (!through) continue;
      const checker = parameter.getProject().getTypeChecker().compilerObject;
      const defaults = new Set(
        through.flatMap((named) => (named.default ? [checker.getTypeFromTypeNode(named.default)] : []))
      );
      let member: Type;
      try {
        member = property.getTypeAtLocation(parameter);
      } catch {
        continue;
      }
      if (assigned) {
        // A member set by assignment is its own payload: the type the route
        // filled, or the declared default when it filled nothing.
        if (defaults.has(member.compilerType)) continue;
        if (payload === undefined) payload = member;
        else if (payload.compilerType !== member.compilerType) return 'abstain';
        continue;
      }
      for (const signature of member.getCallSignatures()) {
        // A payload the member's own type parameter types is whatever each
        // call passes: the parameter states nothing.
        if (signature.getTypeParameters().length > 0) continue;
        const at = this.payloadAt(signature.getParameters(), slot, parameter);
        if (!at) continue;
        // The type parameter at its declared default: the route filled
        // nothing there (a status-code send left at `number`).
        if (defaults.has(at.compilerType)) continue;
        if (payload === undefined) payload = at;
        else if (payload.compilerType !== at.compilerType) return 'abstain';
      }
    }
    if (!found) return 'none';
    return payload ?? 'none';
  }

  /** The payload slot a labelled send member takes, by any of its declarations. */
  private sendSlotOf(declarations: readonly ts.Node[]): ClaimSlot | undefined {
    for (const declaration of declarations) {
      const slot = this.labels.sends.get(declarationKey(declaration));
      if (slot) return slot;
    }
    return undefined;
  }

  /** The payload slot a member set by assignment takes, by any of its declarations. */
  private setSlotOf(declarations: readonly ts.Node[]): ClaimSlot | undefined {
    for (const declaration of declarations) {
      const slot = this.labels.sets.get(declarationKey(declaration));
      if (slot) return slot;
    }
    return undefined;
  }

  /**
   * The type parameters of the type that declares the member that its
   * payload is declared through: a slot each route fills. None for a payload
   * declared with a fixed type (a file path), which says nothing about any
   * one route.
   */
  private payloadTypeParameters(declaration: ts.Node, slot: ClaimSlot): ts.TypeParameterDeclaration[] {
    const owner = declaration.parent;
    const ownParameters = new Set<ts.Node>(
      owner && (ts.isInterfaceDeclaration(owner) || ts.isClassDeclaration(owner) || ts.isClassExpression(owner))
        ? (owner.typeParameters ?? [])
        : []
    );
    if (ownParameters.size === 0) return [];
    let typeNode: ts.TypeNode | undefined;
    if (ts.isMethodSignature(declaration) || ts.isMethodDeclaration(declaration)) {
      typeNode = declaration.parameters[slot.arg]?.type;
    } else if (ts.isPropertySignature(declaration) || ts.isPropertyDeclaration(declaration)) {
      typeNode = declaration.type;
    }
    if (!typeNode) return [];
    const mentioned = new Set<ts.TypeParameterDeclaration>();
    const visit = (node: ts.Node): void => {
      if (ts.isTypeReferenceNode(node) && ts.isIdentifier(node.typeName)) {
        const name = node.typeName.text;
        for (const parameter of ownParameters) {
          if (ts.isTypeParameterDeclaration(parameter) && parameter.name.text === name) mentioned.add(parameter);
        }
      }
      ts.forEachChild(node, visit);
    };
    visit(typeNode);
    return [...mentioned];
  }

  /** The type the call signature takes at the payload slot, read at `at`. */
  private payloadAt(
    parameters: readonly TsSymbol[],
    slot: ClaimSlot,
    at: ParameterDeclaration
  ): Type | undefined {
    const symbol = parameters[slot.arg];
    if (!symbol) return undefined;
    const declaration = symbol.getDeclarations()[0];
    let type = symbol.getTypeAtLocation(at);
    if (declaration && Node.isParameterDeclaration(declaration) && declaration.isRestParameter()) {
      if (!type.isTuple()) return undefined;
      const first = type.getTupleElements()[0];
      if (!first) return undefined;
      type = first;
    }
    if (slot.key !== undefined) {
      const property = type.getProperty(slot.key);
      if (!property) return undefined;
      type = property.getTypeAtLocation(at);
    }
    const optional =
      declaration !== undefined && Node.isParameterDeclaration(declaration) && declaration.isOptional();
    if (optional && type.isUnion()) {
      const defined = type.getUnionTypes().filter((t) => !t.isUndefined());
      if (defined.length === 1) return defined[0];
    }
    return type;
  }

  /**
   * What the labels say about one call (carrick#2472 C2): the payload slot
   * when the called member's declaration is a labelled send; `'setter'` when
   * the receiver's type has a labelled send member but this member is not
   * one (a header, cookie or status setter that returns the reply too);
   * `undefined` when no label speaks, so the caller keeps its own reading.
   * Matched by declaration, never by name; the receiver's properties are read
   * as `statedPayload` reads them, so an inherited send counts.
   */
  sendAt(call: Node): ClaimSlot | 'setter' | undefined {
    if (this.labels.sends.size === 0) return undefined;
    if (Node.isNewExpression(call)) return this.constructSlotOf(call);
    if (!Node.isCallExpression(call)) return undefined;
    const callee = call.getExpression();
    if (!Node.isPropertyAccessExpression(callee)) return undefined;
    const slot = this.sendSlotOf((callee.getNameNode().getSymbol()?.getDeclarations() ?? []).map((d) => d.compilerNode));
    if (slot) return slot;
    let receiver: Type;
    try {
      receiver = callee.getExpression().getType().getNonNullableType();
    } catch {
      return undefined;
    }
    const repliesHere = receiver
      .getProperties()
      .some((property) => this.sendSlotOf(property.getDeclarations().map((d) => d.compilerNode)) !== undefined);
    return repliesHere ? 'setter' : undefined;
  }

  /**
   * The payload slot of a labelled `new` send (carrick#3053): the constructed
   * expression's construct signatures are keyed by the declaration the
   * verifier read (an inherited constructor is the base's). A signature with
   * no declaration (a class that writes no constructor and has no base to
   * borrow one from) has no key; the verifier never labels one with a payload
   * slot either, so it reads as no label and the structural reading decides.
   */
  private constructSlotOf(construct: Node): ClaimSlot | undefined {
    if (!Node.isNewExpression(construct)) return undefined;
    let declarations: ts.Node[];
    try {
      declarations = construct
        .getExpression()
        .getType()
        .getConstructSignatures()
        .flatMap((signature) => {
          const declaration = signature.compilerSignature.getDeclaration();
          return declaration ? [declaration] : [];
        });
    } catch {
      return undefined;
    }
    return this.sendSlotOf(declarations);
  }

  /**
   * The calls of a labelled send member the handler makes on `parameter`, or
   * on a call chain rooted at it (`res.status(201).json(body)`), each with
   * its payload slot and the keys of the member's declarations.
   */
  private sendsThrough(
    handler: HandlerFunction,
    parameter: ParameterDeclaration
  ): Array<{ call: CallExpression; slot: ClaimSlot; keys: string[] }> {
    const name = parameter.getNameNode();
    const symbol = Node.isIdentifier(name) ? name.getSymbol() : undefined;
    const body = handler.getBody();
    if (!symbol || !body) return [];
    const sends: Array<{ call: CallExpression; slot: ClaimSlot; keys: string[] }> = [];
    for (const call of body.getDescendantsOfKind(SyntaxKind.CallExpression)) {
      const callee = call.getExpression();
      if (!Node.isPropertyAccessExpression(callee)) continue;
      const declarations = (callee.getNameNode().getSymbol()?.getDeclarations() ?? []).map((d) => d.compilerNode);
      const slot = this.sendSlotOf(declarations);
      if (!slot) continue;
      if (this.rootSymbol(callee.getExpression()) !== symbol) continue;
      sends.push({ call, slot, keys: declarations.map(declarationKey) });
    }
    return sends;
  }

  /**
   * The assignments `<root chain>.<member> = value` the handler makes through
   * the parameter to a member a `set` label names, each with the keys of the
   * member's declarations.
   */
  private setsThrough(
    handler: HandlerFunction,
    parameter: ParameterDeclaration
  ): Array<{ assignment: BinaryExpression; keys: string[] }> {
    const name = parameter.getNameNode();
    const symbol = Node.isIdentifier(name) ? name.getSymbol() : undefined;
    const body = handler.getBody();
    if (!symbol || !body || this.labels.sets.size === 0) return [];
    const sets: Array<{ assignment: BinaryExpression; keys: string[] }> = [];
    for (const assignment of body.getDescendantsOfKind(SyntaxKind.BinaryExpression)) {
      if (assignment.getOperatorToken().getKind() !== SyntaxKind.EqualsToken) continue;
      const left = assignment.getLeft();
      if (!Node.isPropertyAccessExpression(left)) continue;
      const declarations = (left.getNameNode().getSymbol()?.getDeclarations() ?? []).map((d) => d.compilerNode);
      if (!this.setSlotOf(declarations)) continue;
      if (this.rootSymbol(left.getExpression()) !== symbol) continue;
      sets.push({ assignment, keys: declarations.map(declarationKey) });
    }
    return sets;
  }

  /**
   * The status a reply set by assignment states (carrick#3052): the nearest
   * earlier statement of the same block that assigns a number to another
   * member of the same root and states a status. `undecided` when none does.
   * No member name is consulted. An assignment of a number to any member
   * counts, so `ctx.length = 200` before a body reads as a 200 status.
   */
  private assignedStatus(assignment: BinaryExpression): ReturnType<ValueStatus> {
    const statement = assignment.getParent();
    if (!Node.isExpressionStatement(statement)) return 'undecided';
    const root = this.rootSymbol(assignment.getLeft());
    if (!root) return 'undecided';
    for (const earlier of [...statement.getPreviousSiblings()].reverse()) {
      if (!Node.isExpressionStatement(earlier)) continue;
      const set = earlier.getExpression();
      if (!Node.isBinaryExpression(set) || set.getOperatorToken().getKind() !== SyntaxKind.EqualsToken) continue;
      const left = set.getLeft();
      if (!Node.isPropertyAccessExpression(left) || this.rootSymbol(left) !== root) continue;
      if (this.setSlotOf((left.getNameNode().getSymbol()?.getDeclarations() ?? []).map((d) => d.compilerNode))) continue;
      const value = set.getRight();
      if ((value.getType().getFlags() & ts.TypeFlags.NumberLike) === 0) continue;
      const status = this.valueStatus(value);
      if (status !== 'undecided') return status;
    }
    return 'undecided';
  }

  /**
   * The success values the handler assigns: each right side's type, the ones
   * after an error or redirect status dropped. `abstain` when a value is
   * typed `any` or `unknown`, which says nothing of what is sent.
   */
  private assignedPayloads(sets: ReadonlyArray<{ assignment: BinaryExpression }>): Type[] | 'abstain' {
    const sent: Type[] = [];
    for (const { assignment } of sets) {
      const status = this.assignedStatus(assignment);
      if (status === 'error' || status === 'redirect') continue;
      const type = this.payloadNode(assignment.getRight()).getType();
      if (type.isAny() || type.isUnknown()) return 'abstain';
      // No body: clearing the reply states no payload.
      if (type.isNull() || type.isUndefined()) continue;
      if (!sent.some((each) => each.compilerType === type.compilerType)) sent.push(type);
    }
    return sent;
  }

  /**
   * The payloads the sends carry whose chain states a success status or
   * none. A value typed `any` or `unknown` is assignable to every variant, so
   * it tells no success body from an error body and is left out.
   */
  private successPayloads(sends: ReadonlyArray<{ call: CallExpression; slot: ClaimSlot }>): Type[] {
    const sent: Type[] = [];
    for (const { call, slot } of sends) {
      const status = this.chainStatus(call);
      if (status !== undefined && status !== 'success') continue;
      let argument = call.getArguments()[slot.arg];
      if (!argument || Node.isSpreadElement(argument)) continue;
      if (slot.key !== undefined) {
        if (!Node.isObjectLiteralExpression(argument)) continue;
        const property = argument.getProperty(slot.key);
        if (!property || !Node.isPropertyAssignment(property)) continue;
        const initializer = property.getInitializer();
        if (!initializer) continue;
        argument = initializer;
      }
      const type = argument.getType();
      if (type.isAny() || type.isUnknown()) continue;
      sent.push(type);
    }
    return sent;
  }

  /** The symbol at the root of a member or call chain (`res.status(201)` is `res`). */
  private rootSymbol(expression: Node): TsSymbol | undefined {
    let node = expression;
    for (;;) {
      if (Node.isPropertyAccessExpression(node) || Node.isCallExpression(node) || Node.isParenthesizedExpression(node) || Node.isNonNullExpression(node)) {
        node = node.getExpression();
        continue;
      }
      break;
    }
    return Node.isIdentifier(node) ? node.getSymbol() : undefined;
  }

  /** The member of the parameter's type whose declaration a label names. */
  private labelledMember(
    parameter: ParameterDeclaration,
    labelled: (key: string) => boolean
  ): TsSymbol | undefined {
    let type: Type;
    try {
      type = parameter.getType();
    } catch {
      return undefined;
    }
    return type
      .getProperties()
      .find((property) => property.getDeclarations().some((d) => labelled(declarationKey(d.compilerNode))));
  }
}
