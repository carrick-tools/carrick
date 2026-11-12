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
import { Node, type ArrowFunction, type CallExpression, type FunctionDeclaration, type FunctionExpression, type MethodDeclaration, type ParameterDeclaration, type Type } from 'ts-morph';
import { type LabelledMembers } from './library-claims.js';
import type { ClaimSlot } from './types.js';
/** A function a handler can be: what the inferrer follows a registration to. */
export type HandlerFunction = FunctionDeclaration | ArrowFunction | FunctionExpression | MethodDeclaration;
/** What a handler's parameters state about the request body. */
export type StatedRequest = {
    kind: 'body';
    type: Type;
    at: ParameterDeclaration;
} | {
    kind: 'no_body';
    at: ParameterDeclaration;
    detail: string;
};
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
export declare class StatedParams {
    private readonly labels;
    /** True for a file the repo declares itself (not a library's or the runtime's). */
    private readonly isRepoFile;
    private readonly chainStatus;
    private readonly valueStatus;
    private readonly payloadNode;
    constructor(labels: LabelledMembers, 
    /** True for a file the repo declares itself (not a library's or the runtime's). */
    isRepoFile: (file: Node) => boolean, chainStatus: ChainStatus, valueStatus: ValueStatus, payloadNode: PayloadNode);
    /** No label names a member: every reading answers nothing. */
    get isEmpty(): boolean;
    /** Whether some label names a reply's send member, called or set by assignment. */
    get readsReplies(): boolean;
    /** Whether some label says a route's handler returns the reply (carrick#3054). */
    get readsReturns(): boolean;
    /**
     * The request body the handler's parameters state, the first parameter
     * whose type has a labelled body member deciding. Undefined when no
     * parameter has one, or the one that does states nothing decidable. A
     * route whose `registration` hands in another function beside the handler
     * (a middleware, which can read the body first) decides no "no body".
     */
    request(handler: HandlerFunction, registration?: Node): StatedRequest | undefined;
    /**
     * The route hands in a function besides the handler: a callable argument
     * (or list of them) beside the handler on any call it is passed through
     * up to the registration, or, where the registration names the handler by
     * reference, a second callable argument or property there. What such a
     * function (a middleware) does with the request is not read here.
     */
    handsInAnotherFunction(registration: Node, handler: HandlerFunction): boolean;
    /**
     * A type argument the source writes for this route: on a call the handler
     * is passed to (the registration, or a wrapper that makes the handler),
     * through any calls enclosing that one as an argument, or on the
     * parameter's own annotation.
     */
    private writesTypeArgument;
    /**
     * Nothing reads the labelled body member off parameter `index` of
     * `func`, nor off the request in the repo functions it is handed to, up to
     * `REACH_DEPTH` calls deep. Every use of the request must be a member
     * read that is not the body and is not itself called (a framework's own
     * body reader), or a hand-off to a function the repo declares, whole and
     * by position. Anything else (a destructured parameter, a library
     * function, an alias, a spread) is a read this cannot rule out.
     */
    private bodyIsUnread;
    /**
     * A read of a request member (`req.headers`, `req.headers.host`) whose
     * value goes nowhere a body could be read from. Along its member chain, a
     * method called on anything but a plain value (`req.raw.pipe(...)`, or a
     * framework's own body reader `req.file()`) is a read, and so is a
     * non-plain value handed to a function (`getRawBody(req.raw)`). A plain
     * value (a header string) holds no body, whatever is done with it.
     */
    private memberReadStaysPut;
    /** The function a callee names, where the repo declares it; undefined otherwise. */
    private repoFunction;
    /**
     * The reply the handler's parameters state, cut to its success variants.
     * Undefined when no parameter has a labelled send member that states a
     * payload, when two such members disagree, when the payload is `any`,
     * `unknown` or `never`, or when a union of bodies has no variant a
     * success send through the parameter carries.
     */
    reply(handler: HandlerFunction): StatedReply | undefined;
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
    returned(registration: Node, handler: HandlerFunction): StatedReply | undefined;
    /** The `returns` label of the route member a registration call calls, by its declarations. */
    private returnsLabelOf;
    private unions;
    /** The parts of `type`, each read through its promise. */
    private settled;
    /** A promise read through to what it resolves to, at most four hops. */
    private awaited;
    private isNothing;
    /**
     * The payload the labelled send members the handler calls through the
     * parameter state: `none` when it calls none, `abstain` when they state
     * different payloads, else the one payload type. A member the handler never
     * calls (a reply object's status-code send beside its body send) says
     * nothing about this route.
     */
    private statedPayload;
    /** The payload slot a labelled send member takes, by any of its declarations. */
    private sendSlotOf;
    /** The payload slot a member set by assignment takes, by any of its declarations. */
    private setSlotOf;
    /**
     * The type parameters of the type that declares the member that its
     * payload is declared through: a slot each route fills. None for a payload
     * declared with a fixed type (a file path), which says nothing about any
     * one route.
     */
    private payloadTypeParameters;
    /** The type the call signature takes at the payload slot, read at `at`. */
    private payloadAt;
    /**
     * The called member's declaration is one the labels name a request body
     * (blueprint row N33): a framework's own `json()` on its request, read as
     * the body where the store says so. Matched by declaration, never by name.
     */
    readsLabelledBody(call: Node): boolean;
    /**
     * What the labels say about one call (carrick#2472 C2): the payload slot
     * when the called member's declaration is a labelled send; `'setter'` when
     * the receiver's type has a labelled send member but this member is not
     * one (a header, cookie or status setter that returns the reply too);
     * `undefined` when no label speaks, so the caller keeps its own reading.
     * Matched by declaration, never by name; the receiver's properties are read
     * as `statedPayload` reads them, so an inherited send counts.
     */
    sendAt(call: Node): ClaimSlot | 'setter' | undefined;
    /**
     * The payload slot of a labelled `new` send (carrick#3053): the constructed
     * expression's construct signatures are keyed by the declaration the
     * verifier read (an inherited constructor is the base's). A signature with
     * no declaration (a class that writes no constructor and has no base to
     * borrow one from) has no key; the verifier never labels one with a payload
     * slot either, so it reads as no label and the structural reading decides.
     */
    private constructSlotOf;
    /**
     * The calls of a labelled send member the handler makes on `parameter`, or
     * on a call chain rooted at it (`res.status(201).json(body)`), each with
     * its payload slot and the keys of the member's declarations.
     */
    private sendsThrough;
    /**
     * The assignments `<root chain>.<member> = value` the handler makes through
     * the parameter to a member a `set` label names, each with the keys of the
     * member's declarations.
     */
    private setsThrough;
    /**
     * The status a reply set by assignment states (carrick#3052): the nearest
     * earlier statement of the same block that assigns a number to another
     * member of the same root and states a status. `undecided` when none does.
     * No member name is consulted. An assignment of a number to any member
     * counts, so `ctx.length = 200` before a body reads as a 200 status.
     */
    private assignedStatus;
    /**
     * The success values the handler assigns: each right side's type, the ones
     * after an error or redirect status dropped. `abstain` when a value is
     * typed `any` or `unknown`, which says nothing of what is sent.
     */
    private assignedPayloads;
    /**
     * The payloads the sends carry whose chain states a success status or
     * none. A value typed `any` or `unknown` is assignable to every variant, so
     * it tells no success body from an error body and is left out.
     */
    private successPayloads;
    /** The symbol at the root of a member or call chain (`res.status(201)` is `res`). */
    private rootSymbol;
    /** The member of the parameter's type whose declaration a label names. */
    private labelledMember;
}
