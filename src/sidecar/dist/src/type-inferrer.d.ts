/**
 * Type Inferrer - Scope-based type inference with payload unwrapping
 *
 * This module extracts types even when developers don't write explicit
 * annotations. It uses span-based node lookup (no line windows) to target
 * precise expressions provided by the Rust/LLM pipeline.
 *
 * Key feature: Agent-informed payload unwrapping
 * - Extracts payload types from machinery wrappers (Response<T>, AxiosResponse<T>, etc.)
 * - Supports union/intersection composition
 * - Recursive unwrapping with depth limits
 *
 * Framework agnosticism: The LLM emits the payload subexpression directly (e.g.,
 * the `users` in `res.json(users)`, `ctx.body = users`, `h.response(users)`, or
 * `return users`). The sidecar resolves that node and reads its type — no
 * framework-specific method-name lists live here. For payload-less handlers
 * (redirects, 204s), the LLM emits null and we fall back to the containing
 * function's return type.
 */
import { Project, SourceFile, type CallExpression } from 'ts-morph';
import type { InferRequestItem, InferResult, ModelSymbol, ExtractionConfig } from './types.js';
import { type LabelledMembers } from './library-claims.js';
/**
 * Strongly-discriminating member names of HTTP transport machinery — the
 * fetch/DOM `Response` & `Request`, a Node `http.ServerResponse`, a framework
 * reply object. A type declared in a lib or installed-dependency origin that carries
 * a subset of these is framework machinery, never a user contract: the
 * PRODUCER-side structural mirror of the consumer `machineryIndicators`
 * (ExtractionConfig), used to reject a wrapper envelope whose response field is
 * a raw `Response` (carrick#371) instead of emitting it as a comparable type.
 *
 * Framework-agnostic by construction: no framework NAME appears here, only the
 * shared HTTP-message surface. The names are deliberately the ones no
 * JSON payload ever carries (`ok`, `redirected`, `bodyUsed`, `arrayBuffer`,
 * `writeHead`, ...), so the origin gate + `MACHINERY_INDICATOR_THRESHOLD` never
 * fire on real data. Kept in lockstep with the capture-seam mirror
 * `capture/machinery.ts` (the seam forbids sharing a module across it, same as
 * `BUILTIN_ANCHOR_SYMBOLS` mirrors `socket_io.rs`). Exported so a drift-guard
 * test (`machinery-indicator-mirror.test.ts`) asserts the two sets stay equal.
 */
export declare const MACHINERY_MEMBER_INDICATORS: Set<string>;
/**
 * `text` is `string`, alone or beside `null` and `undefined` in either order:
 * the only payload a raw-text read publishes (carrick#1842).
 */
/**
 * Source text with every plain string literal written in double quotes:
 * `'/a'`, `"/a"` and an interpolation-free `` `/a` `` are one value, so text
 * that differs only in its quotes compares equal (carrick#2056).
 *
 * The text is read left to right, pairing quotes the way the scanner does,
 * so a template's closing backtick never pairs with the next template's
 * opening one. A literal holding a quote, a backslash or an interpolation is
 * copied as it is, and so is an unterminated one with everything after it.
 */
export declare function canonicalStringQuotes(text: string): string;
/**
 * Options for TypeInferrer construction
 */
export interface TypeInferrerOptions {
    /** The ts-morph Project instance */
    project: Project;
    /**
     * The registry package a resolved file belongs to, when the module graph
     * that built the project names one (carrick#1260). Supplied for a project
     * whose resolution went through a graph rather than through `node_modules`;
     * absent otherwise, and the path itself is then the only thing to read.
     */
    packageOf?: (filePath: string) => string | undefined;
    /**
     * The service root the project was built for. The machinery origin gate
     * reads it to tell a workspace package the compiler reached through a
     * `node_modules` symlink from an installed dependency (carrick#1264).
     */
    repoRoot: string;
    /**
     * How much work the unwidened reading of one `infer` batch may do
     * (carrick#2629). Each field defaults to its `UNWIDENED_*` constant.
     */
    unwidenedLimits?: Partial<UnwidenedLimits>;
    /**
     * The clock the unwidened reading's last-resort time limit reads, in
     * milliseconds. Defaults to `performance.now`; a test passes a slow one.
     */
    now?: () => number;
}
/**
 * The bounds on one batch's unwidened reading (carrick#2629). The first two
 * count work, so a batch reads the same on a quiet machine and a loaded one;
 * the third is a wall-clock backstop for a reading that runs away.
 */
export interface UnwidenedLimits {
    /** Readings (requests re-read in the rewrite) one batch may make. */
    readings: number;
    /** Files one batch may type-check before and after its rewrite. */
    files: number;
    /** The longest the reading may run, from its start, before it stops. */
    backstopMs: number;
}
/**
 * Called each time an `infer` batch is done with one of its requests,
 * whatever that request answered: skipped, refused, failed or inferred.
 */
export type InferRequestDone = () => void;
/**
 * Called each time the unwidened reading finishes a unit of its work: a file
 * type-checked or a request re-read (carrick#2629). The reading runs after
 * every request is done, so this is what keeps a long one from going silent.
 */
export type UnwidenedStepDone = () => void;
export declare class TypeInferrer {
    private readonly project;
    private readonly packageOf;
    private readonly repoRoot;
    /**
     * The node each inferred type was read from, keyed by the location object
     * `getNodeLocation` built for it (the one an `InferredType` carries). The
     * unwidened reading re-reads from here (carrick#1516).
     */
    private readonly readNodes;
    private readonly unwidenedLimits;
    private readonly now;
    /**
     * The files a request has named to this inferrer, as the requests named
     * them (carrick#1985). It lives as long as the project does, so the first
     * request of a file is the first of the process, not of a batch.
     */
    private readonly filesAsked;
    constructor(options: TypeInferrerOptions);
    /**
     * What the structural printer needs to tell the user's own declarations from
     * the runtime's and its packages' — the same program and service root this
     * class asks `isExternalOrigin` about machinery, so the two layers cannot
     * classify one declaration two ways.
     */
    private expandOrigin;
    /**
     * Infer types for the given requests
     *
     * @param requests - Array of inference requests
     * @param extractionConfig - Agent-generated extraction config for payload unwrapping
     * @param onRequestDone - Called once per request, as the batch is done with it
     * @param onUnwidenedStep - Called once per unit of the unwidened reading's work
     * @returns InferResult with inferred types or errors
     */
    infer(requests: InferRequestItem[], extractionConfig?: ExtractionConfig, onRequestDone?: InferRequestDone, modelSymbols?: ModelSymbol[], labels?: LabelledMembers, onUnwidenedStep?: UnwidenedStepDone, frameworkPackages?: readonly string[]): InferResult;
    /** The types the model named, by the alias of the row each is for (carrick#2111). */
    private modelSymbols;
    /**
     * carrick#2674: what route handlers' parameters state, through the
     * members the batch's verified server-framework labels name. Empty when
     * the batch carries no labels, and then it reads nothing.
     */
    /**
     * The packages the store labels a server framework for this batch (N8),
     * by the name a service imports them as.
     */
    private frameworkPackages;
    private statedParams;
    /**
     * carrick#2111: where the model named a type for the response this request
     * reads, record on `inferred` how that type relates to the compiler's type
     * of what the handler sends (`read`, one type per surviving branch). The
     * driver keeps, re-depths or drops the model's symbol on the answer.
     *
     * Nothing is recorded, and the symbol stands, where there is nothing to
     * compare: the symbol does not resolve in this program, or the published
     * read holds `any` or `unknown` somewhere.
     */
    private relateModelSymbol;
    /**
     * Blueprint row N27 (i) (carrick#3334): a consumer call whose result is
     * untyped (`client.post(url, event)` on an untyped client) states nothing
     * the model's symbol could be compared with, and the model sometimes names
     * the type of what the call SENDS. Where the symbol is the type of one of
     * the call's arguments, or that argument's element, the answer is
     * `different`: the symbol names the request, not the result, and the
     * driver drops it so the inference defines the alias.
     *
     * Nothing is recorded where the result (`read`, the type the published
     * text prints) is typed: a consumer's read can be a later expression of
     * the caller's, so a typed result is not related here.
     */
    private relateModelSymbolToArguments;
    /**
     * `func` is the function the row's line names: it opens on that line (a
     * method under its decorators, an exported handler), or it is the handler
     * of the registration that opens there (`app.post<…>(\n '/path',\n async
     * (request) => { … })`, or a named handler declared elsewhere).
     */
    private isTheRowsOwnFunction;
    /**
     * The declared type of a symbol the model named, found as the bundle finds
     * it (through a barrel's re-exports), in a file this program already
     * holds. A name that declares a value and no type resolves to nothing.
     */
    private modelSymbolType;
    /**
     * carrick#1836: list on `result` the declarations behind the names its text
     * prints that the request's file cannot resolve (`PrintedTypes.namesIn`).
     */
    /** `inferSingle`, with what the names it prints meant (carrick#1836). */
    private inferRecordingNames;
    /**
     * carrick#1029: the loop element a route row's path names, when its
     * registration is made once per element of a constant list
     * (`loop-element.ts`). Only a route row that states its path is read.
     */
    private loopPickFor;
    /**
     * The request inferred with the loop's list rewritten to the one element
     * `pick` names, so the compiler types the handler for that element. The
     * file is restored before returning, and the names the reading prints are
     * recorded while the rewritten program is still the project's. Lines do not
     * move (the rewrite is padded); a span after the list moves by the length
     * the rewrite changed.
     */
    private inferForLoopElement;
    private recordPrintedNames;
    /**
     * carrick#1516: read each response inference again with the literals on its
     * handler's path marked `as const`, and record the narrower type the handler
     * really returns beside the published one (`unwidened_type_string`). See
     * `unwidened.ts` for what is marked and why the reading is sound.
     *
     * carrick#2011: a request body (`read` is `'request_body'`) is read the same
     * way, the call that sends it in place of the handler, except that a literal
     * handed to any call other than the send is never marked.
     *
     * One rewrite for the whole batch: every file on any request's path is
     * rewritten once, type-checked before and after, the requests re-inferred,
     * and every file restored before returning, so the project other requests
     * read is the project the scan loaded.
     *
     * The reading is bounded by work, not time (carrick#2629): a batch makes at
     * most `UnwidenedLimits.readings` readings over at most `.files` files,
     * taken in request order, the responses first. The first request past
     * either cap and every request after it are marked
     * `unwidened_out_of_budget` (carrick#2100) and publish no reading, on any
     * machine. A reading still running at the wall-clock backstop stops the
     * same way and says so in the log. A request that never needed a reading
     * (a declared return, nothing to mark) or lost it for a reason of its own
     * (a marking that added a diagnostic) is never marked.
     */
    private addUnwidenedReadings;
    /**
     * The request as it locates in the rewritten files: a span moved past the
     * insertions before it, an expression text replaced by the rewritten text of
     * the node it named. Lines do not move (an insertion holds no newline).
     */
    private mappedRequest;
    /**
     * Infer a single type from a request
     */
    private inferSingle;
    /**
     * The call a `call_result` locator names, located exactly as `infer` locates
     * it (span, else expression text near a line). The retype check
     * (carrick#1491) rewrites this node, so it must be the node the consumer's
     * published type came from, not a guess at the line.
     */
    locateCall(request: InferRequestItem): {
        sourceFile: SourceFile;
        call: CallExpression;
    } | undefined;
    /**
     * Get or add a source file to the project
     */
    private getSourceFile;
    /**
     * carrick#2824: a located response expression that sits in ANOTHER route's
     * handler is not this row's. A model can locate `response` at a line that
     * belongs to the next registration in the same module; reading that node
     * publishes the other route's reply. Returns the request with its locator
     * dropped, so it is answered by the registration on its own line, when:
     *  1. a registration R sits on the request's line;
     *  2. the located node is outside R and outside R's handler;
     *  3. some ancestor of the node is a registration whose handler contains it.
     * Anything else is returned unchanged. Matches on node shape, no names.
     */
    private withoutForeignLocator;
    private inferFunctionReturn;
    /**
     * Build a `function_return` inferred type from an already-resolved function
     * node: unwrap `Promise<…>` / async-iterator transport, apply the extraction
     * config to the awaited type, and expand a bare named return structurally.
     * Extracted from `inferFunctionReturn` so the route-registration path
     * (`inferResponseBody` following a handler) can reuse the exact same logic
     * against the handler function instead of the containing function.
     */
    private buildFunctionReturnInferredType;
    /**
     * Infer a function's return type for the signature hint. Unlike
     * `inferFunctionReturn`, this does NOT unwrap Promise or apply wrapper
     * rules — a function that returns `Promise<AuthResult>` should show exactly
     * that in its signature. Used by the function-signature collection pass.
     */
    private inferSignatureReturn;
    /**
     * Infer the type of a single named parameter. `is_explicit` reflects whether
     * the parameter carries a source annotation; the type string is the
     * compiler's view either way (so contextually-typed callback params resolve
     * even without an annotation). Uses ts-morph's default `getText()` form,
     * which keeps named types as names and bounds depth via the compiler's own
     * truncation.
     */
    private inferFunctionParam;
    /**
     * carrick#2814: mark a route's request body as one its handler tolerates
     * going unsent, when the handler is in reach and every read of the body
     * survives `undefined` (`absent-body.ts`). The check then lets a caller that
     * may send nothing call a route whose body type has no required member.
     *
     * No mark where the handler cannot be found, where a validator the
     * registration declares reads the body first (a schema, or another function
     * handed in beside the handler), or where the handler reads no body it can
     * see: the check keeps the reading it had.
     */
    private withAbsentBodyMark;
    /**
     * Resolve a `function_param` locator against a function's parameter list.
     * Three shapes, tried in order:
     *
     * 1. A parameter whose name matches exactly (`(payload) => …` ← "payload").
     * 2. A parameter whose DESTRUCTURED BINDING PATTERN matches the locator
     *    text under whitespace normalization (`({ time, run }) => …` ←
     *    "{ time, run }") — the handler destructures the payload itself, so the
     *    pattern's own type IS the payload type.
     * 3. A named BINDING ELEMENT inside a destructured parameter
     *    (`({ payload }) => …` ← "payload") — the payload is one property of an
     *    envelope param, and the checker projects the element's type
     *    (catalog-worker handlers: `params: { id, payload: Infer<…> }`).
     *
     * All three read the type off a node the checker has already instantiated,
     * so generic wrappers (topic-map emitters, schema catalogs, channel handles)
     * resolve without any named payload symbol existing anywhere.
     */
    private resolveParamTarget;
    private inferResponseBody;
    /**
     * The handler a module exports on `line`: a function that opens on exactly
     * that line, exported at the top of its module, where no call on the line
     * registers a route (carrick#807).
     *
     * That is the anchor of a route its module's place in the project states:
     * the row's line is the handler's own export, and the module's exports are
     * how the framework finds it. Each condition keeps another kind of row out:
     *
     *  - the line has to be the function's first, with no tolerance. A model
     *    row that names no site of its own can sit on any line of a handler,
     *    and the nearest function to such a line is whatever is declared
     *    beside it;
     *  - a method of a class is a controller's handler, whose rows are
     *    anchored on what a model located before the method's own return;
     *  - a line that registers a route has a registration to read.
     */
    private handlerDeclaredAtLine;
    /**
     * `handlerDeclaredAtLine` without its registration test, for a caller that
     * has already looked for a registration on the line.
     */
    private exportedFunctionOpeningOn;
    /**
     * The response of a handler the request's line declares, for a request
     * that also located an expression inside it (carrick#807).
     *
     * The handler's own return is read first, exactly as a `function_return`
     * request at the line reads it, and that reading stands: a body it
     * recovered, and a decision it made not to publish one. The located
     * expression is consulted in one case only. Where the handler returns a
     * call nothing resolves (its library is not installed), the return walk
     * reads no argument the source does not annotate, because it cannot tell a
     * response sender from a query. A located payload that is an ARGUMENT of
     * such a returned call is the model's word that it is a sender, and that is
     * a lead, never an enforced type (blueprint row N34): the walk's honest
     * `any` stands, and its `no_payload_evidence` provenance quotes the
     * argument's reading. A send the service's labels name is read by the walk
     * itself (`returnsLabelledSend`), so it never reaches this case.
     *
     * Returns `undefined` when the located expression is something the return
     * walk never looked at, so it is left to the readings below:
     *  - the handler returns nothing (`void`): it sends through a parameter;
     *  - the handler returns transport the walk read no body out of, and the
     *    located expression is not part of what it returns. A body built before
     *    the response is returned (`const res = send(body); ...; return res`)
     *    is one. Named on a send that states an error or redirect status, it is
     *    no success body and the row abstains.
     */
    private responseOfDeclaredHandler;
    /**
     * The call inside one of `returned` that the located expression is, or is
     * an argument of: the send a located payload was handed to. `undefined`
     * when the located expression is not part of what the handler returns.
     */
    private returnedSendHolding;
    /**
     * True when `node` is a bare reference to a parameter of the handler the
     * route registration on the request's line hands in (found as
     * `registrationAtLine` finds it, wrappers included): the request, the reply
     * or the context the framework passes, never a body the route sends
     * (carrick-cloud#2576). Where the parameter's type is declared is not read:
     * a parameter of any other function is data, whether the repo or an
     * installed package types it (`.then((account) => reply.send(account))`, a
     * helper's `account: Account`).
     */
    private isRouteHandlerParameter;
    /**
     * True when a located call's own result is the route's payload, so the
     * transitional drill into its first argument must not run (carrick#1732).
     *
     * `res.json(users)` reached that drill because nothing above it recognised
     * the send: its result reads `void`, `any` or `unknown`, and the payload is
     * the argument. `toPublicView(row)` is the opposite case: a mapper building
     * the object the route sends. Its first argument is the row it was built
     * FROM, which carries columns the route never sends.
     *
     * The call's result decides, not where its callee is declared. It is the
     * payload when it reads as one by the rule a response helper's argument is
     * read with (`nodeCarriesPayloadContract`: object-shaped, not machinery,
     * not `void`/`any`/`unknown`) and the object is not a library's own: a
     * codec's writer from `encode(message)` or a reply builder from a send is
     * the library describing itself, and keeps the drill. A library call that
     * returns the repo's own type (`toInstance(View, plain)`) is the payload.
     */
    private callResultIsPayload;
    /**
     * True when a located call evaluates to plain data, so it is the payload
     * whatever its arguments and wherever its type is declared (carrick#2055).
     *
     * `rows.map(toDto)`, `rows.filter(cb)` and `listItems(ownerId)` produce the
     * list a route sends. Read as a send, the first produced the mapper's
     * function type, the second a callback's return, and the third `string`.
     * What a send or a registration hands back is never plain data: a reply
     * builder or a router carries methods, a bare send returns `void`, and a
     * serialiser returns a primitive. So the result alone tells them apart.
     *
     * Plain data, once promise levels are taken off: an object with at least
     * one member, no call or construct signature, and no member that is itself
     * callable; an array of one; or a union whose members other than `null` and
     * `undefined` all are. Tuples and empty objects are left to the callers'
     * own reading.
     */
    private callResultIsPlainData;
    /**
     * The wire representation a request's printed type takes. A route response
     * is serialised as JSON by every sender this layer reads a payload out of,
     * so it prints `toJSON()` results rather than the objects that declare them
     * (carrick#1163). Everything else prints the declared type.
     */
    private wireFormatFor;
    /** An inferred type for a payload read out of response sends. */
    private inferredFromRecoveredPayload;
    /**
     * carrick#1161: type a route response from every send in the handler the
     * locator anchors, rather than from the one send it names.
     *
     * The analyzer reports one response expression per row, and on a handler
     * that guards before it succeeds that is the guard's body. So the located
     * expression selects the HANDLER: the function that contains it, on a row
     * whose line is a route registration. Its returned sends are enumerated
     * (conditional branches split), each is classified by the status it states
     * (`responseSiteStatus`), the error and redirect branches are dropped, and
     * the success bodies are joined exactly as the payload walk joins return
     * statements.
     *
     * Returns `undefined` when this does not apply, which leaves the caller's
     * own reading of the located expression untouched:
     *  - the row's line is neither a route registration nor a line inside the
     *    handler the located send sits in, unless the located send states an
     *    error or redirect status and the handler keeps another send (carrick#2463);
     *  - the located expression is neither a send nor the argument of one;
     *  - that send is neither one the handler returns nor a statement of it;
     *  - it is a success send the handler does not return (a statement send
     *    joins only when the located send is dropped, and only when it ends
     *    the handler's path);
     *  - it is a success send and the only one that survives, so the join would
     *    be the located body anyway;
     *  - no joined body is the located one (a text body beside JSON ones).
     *
     * When the located send is an error or redirect branch and no success body
     * survives, the route sends no body a contract can describe: an explicit
     * `unknown` with its reason, so no later locator re-run reads the error body
     * or the redirect location back in.
     */
    private inferFromResponseSites;
    /**
     * carrick#2372: what `func` returns when it returns one call into a
     * function of the program: see `callNarrowedByArguments`.
     */
    private returnNarrowedByArguments;
    /**
     * carrick#2372: the one member of `awaited` (the awaited result type of
     * `call`) the called function's returns can give for the arguments `call`
     * passes, and the parameters the callee tests that those arguments state.
     * `undefined` when `call` runs no function of the program (or runs
     * `caller` itself), or its result is not a union.
     *
     * Each kept return is matched to the union members it is assignable to; a
     * return that matches none (another union, a call) keeps every member, as
     * does a narrowing that leaves more than one member.
     */
    private callNarrowedByArguments;
    /**
     * carrick#2372: a union read from an executor whose bodies depend on
     * arguments the route passes, where those arguments could not tell the
     * bodies apart. Each such parameter is listed as an `argument` read, so a
     * mismatch against the union is not reported as this route's: no call can
     * say which body the route sends.
     */
    private withArgumentReads;
    private addArgumentReads;
    /**
     * The response sends `func` makes as statements (`res.status(409).json(e);`),
     * for a handler that sends through a parameter and returns nothing. Only the
     * outermost call of a statement is a send, only in `func`'s own scope, and
     * only where `isSend` accepts it, so a logger call or an `await save()`
     * statement is never one.
     */
    private responseSendStatements;
    /**
     * True when nothing in `func` runs after `statement`: a bare `return;`
     * follows it, or it is the last statement of its block and the statement
     * that block belongs to (an `if` branch, a `try` or `catch` block) is itself
     * last, up to `func`'s body. A statement inside a loop, a `switch` case or
     * any other construct does not end the path; a send there is not read as a
     * success body.
     */
    private statementEndsFunctionPath;
    /**
     * The handler returns a send the service's labels name (carrick#3053): a
     * `new` construction or a static call whose declaration a verified label
     * keys. Its result is transport whatever its type looks like, so the body
     * is read from the send's argument.
     */
    private returnsLabelledSend;
    /**
     * True when `candidate` is a response send: a call or `new` whose result is
     * transport machinery (by the extraction config's verified rule or the
     * structural check).
     */
    private isResponseSend;
    /**
     * The call `node` is an ARGUMENT of, looking through the wrappers that do
     * not change a payload (parentheses, `as`, `satisfies`, `!`, `await`) and a
     * `JSON.stringify` around the body. `undefined` when `accept` rejects that
     * call or `node` is its callee.
     *
     * Two readings use it: a value handed to a response send is the payload that
     * send transmits, and a body read handed to a call that states what it
     * returns is a better statement of that body than the read (carrick#1382).
     */
    private receivingCallOf;
    /**
     * A call's result read through the library store's label (carrick#2818):
     * `whole` is the awaited type, `member` is that member of every non-nullish
     * constituent of it, and `parse` is the awaited return of that method as the
     * call returns it (a thenable's own `json()` is not on what it resolves to).
     * A part the type lacks is a decided abstain, so the capture's own re-read
     * of the raw call does not publish the envelope in its place.
     */
    private labelledCallResult;
    private inferCallResult;
    /**
     * The terminal of a call's def-use walk is a zero-argument `.text()` read of
     * the call's own result (carrick#1842): on its binding (`res.text()`,
     * through a declaration, `await` or a cast), or in the callback a `then` on
     * the call hands the response to (`fetch(u).then((res) => res.text())`).
     */
    private textReadAtTerminal;
    /**
     * `call` is the platform's `<receiver>.text()` (blueprint row N33), and
     * `accept` takes its receiver.
     */
    private isTextReadOf;
    /**
     * Whether a declaring file is the runtime's, as `isExternalOrigin` answers
     * it: the default library, an installed package, or Carrick's materialised
     * runtime declarations.
     */
    private readonly runtimeFile;
    /**
     * `call` is one of the platform's whole-body reads (blueprint row N33).
     * `undeclared` says whether a read on a receiver nothing declares counts.
     */
    isPlatformBodyRead(call: CallExpression, reads: ReadonlySet<string>, undeclared: 'undeclared_reads' | 'platform_only'): boolean;
    /**
     * The call's resolved signature, at this site, types a member of an object
     * argument as exactly the literal `'text'`, and the source passes that
     * literal there (carrick#1842). That is how a request library lets a caller
     * choose a text body from the formats it offers (`{ type: 'text' }`): a
     * generic config instantiated by the literal, or an overload taken by it.
     * A member typed as a wider union (`kind: 'text' | 'image'`) chooses no
     * format, and no member name is read.
     */
    private callChoosesTextBody;
    /**
     * What the source states the body read at `terminal` to be, when the type
     * `extractExplicitTypeFromAncestor` printed for it is stated AT the read
     * (carrick#1749): the read is the operand of that cast, or the initializer
     * of that annotated declaration, through wrappers that leave a value as it
     * is (parentheses, `await`, `!`, another cast). An annotation further out —
     * the declared type of the function the read sits in — describes something
     * else, and is not reported.
     */
    private statedBodyAtRead;
    /**
     * The type node `statedBodyAtRead` reads, for a caller that wants the
     * annotation itself: a consumer's response read reports its root, and a
     * handler's request read prints it (carrick#807). One reading of "stated at
     * the read" for both, so the two cannot disagree about which annotation
     * belongs to a read.
     */
    private typeNodeStatedAtRead;
    /**
     * A wrapper between a body read and the binding it initialises that leaves
     * the read's value as it is: `await`, parentheses, `!`, a cast, `satisfies`.
     */
    private wrapsReadTransparently;
    /**
     * A stated type with `any` or `unknown` written anywhere in it (`unknown`,
     * `Record<string, unknown>`, `{ items: any[] }`) leaves a position open: it
     * is a placeholder the source narrows later (`const data: unknown = await
     * res.json()`, then `data as Entry[]`), not its statement of the body.
     */
    private leavesAPositionOpen;
    /**
     * The named root of a stated body type: `Promise<...>` and `PromiseLike`
     * (the language's await protocol), array levels, parentheses, `readonly`
     * and `| null`/`| undefined` are peeled, and what is left is either a type
     * reference, whose name is the root, or anything else, which has none.
     */
    private statedRoot;
    /**
     * Blueprint row N30 (carrick#3336): how the root's declared type (what
     * bundling the root alone publishes) relates to the type the source states
     * (`as readonly User[]` against `User`: a list, 1 deep). The compiler
     * decides; the printed text is never compared. A generic root's declared
     * type keeps its parameters, so `Page<User>` relates to `Page` as
     * `different`. Absent where nothing could be compared.
     */
    private statedRootRelation;
    private inferVariable;
    private inferExpression;
    private inferRequestBody;
    private resolveCallResultTerminalNode;
    /**
     * carrick#2756: what a `catch` clause does with the response describes a
     * body that failed to read (`catch (e) { log(await res.text()); throw e }`),
     * not the payload, yet it comes after the success read and would win as the
     * last whole read. Where the source uses the tracked value anywhere outside
     * a `catch` clause that does not hold the call, the clause's uses are left
     * out of the walk. Where every use is in such a clause the walk is as it was.
     */
    private catchReadsDeferred;
    /**
     * Where the walk ends on the call's own destructuring binding (`const {
     * tasks } = await api.list()`), the declaration has no type of its own: the
     * checker answers `any` for it. The value it takes apart is the call's, so
     * the call answers (carrick#2112). Not for a transport value: a helper's
     * `{ body, response }` read as text would bypass the capture's machinery
     * guard, so that binding stays unread and the capture answers the row.
     */
    private destructuredValueTerminal;
    /**
     * Between `callExpr` and the return statement around it there is an object
     * or array literal: the call's result is one part of a value the function
     * builds and returns.
     */
    private callIsPartOfReturnedLiteral;
    /**
     * `expr` declares a name for what a call returns when it is handed a
     * tracked name whole (`const data: T = await parseJson(response, 'op')`).
     */
    private handsTrackedValueToACall;
    /**
     * The call whose result `expr` declares or assigns a name for, and the
     * index of its argument that is a tracked name whole, or undefined.
     */
    private callTakingTrackedValue;
    /**
     * `expr` names the result of a call that states the tracked value it takes
     * (`callStatesItsArgument`): a schema validates it, or the callee returns it
     * unchanged. A converter's result describes what it built from the value,
     * not the value (carrick#2595).
     */
    private statesTrackedValue;
    /** `expr` holds a binary whole-body read taken on a tracked name. */
    private holdsBinaryBodyRead;
    /**
     * `func`, the function a call is made in, is the executor of the promise
     * `returnStmt` returns: `return new Promise<T>((resolve, reject) => { … })`
     * (carrick#2282). The promise is recognised by the language's await
     * protocol on the constructed value, never by a name.
     */
    private callIsInReturnedPromiseExecutor;
    /**
     * `expr` declares a name for the tracked value as it is: its initializer,
     * with the wrappers that leave a value what it was peeled (parentheses,
     * `await`, `as`, `satisfies`, `!`), is a tracked name (carrick#2112). The
     * declaration's type is then the value's, as the source restates it.
     */
    private restatesTrackedValue;
    /**
     * True when a member read on the call's result resolves to one of that
     * result's own TYPE ARGUMENTS — the source is unwrapping a generic envelope
     * by hand (`state.data` off a `ResourceState<Envelope>`), and the payload it
     * carries is the instantiation, not the envelope (carrick#1375).
     *
     * The generic is what tells the two apart. A call that answers its payload
     * directly is read member by member too, and its declared result IS the
     * contract; abstaining there would throw away the type the request boundary
     * states, which a replay over a real repo's consumer rows showed on a
     * `{ ok: true } | { ok: false; reason: string }` result read as `sent.ok`.
     */
    private projectionReadsGenericPayload;
    /**
     * True when the call answers a ONE-ARGUMENT result envelope: a union whose
     * branches say whether the call worked and one of which holds the single
     * type argument as a member (`{ success: true; data: T } | { success: false;
     * error: string }`), carrick#3741. carrick#1376 needs two arguments to tell
     * the success side from the failure side; with one there is nothing to tell
     * apart, and the envelope is the client's bookkeeping around `T`.
     *
     * A site that reads only such an envelope's own members (its status, its
     * error) reads no payload, so the envelope is not a response contract. A
     * union with no type argument (`{ ok: true } | { ok: false; reason: string }`)
     * is not an envelope here and keeps its declared answer (carrick#1375).
     */
    private isOneArgumentEnvelope;
    /**
     * The payload a RESULT CARRIER carries, or `undefined` when `type` is not
     * one or its success side cannot be told from its failure side
     * (carrick#1376).
     *
     * A carrier is recognised by its shape, never by a name: a union of object
     * branches, instantiated with two or more type arguments, at least one of
     * which a branch holds as a member. `Result<T, E>`, `Either<L, R>` and a
     * hand-rolled `{ ok: true; value: T } | { ok: false; error: E }` are all the
     * same shape, and a promise-like around one is peeled first through the
     * language's own await protocol. A single generic object — a resource state,
     * a query result — is NOT a union and is left to carrick#1375, which
     * abstains on it so a sibling site can answer.
     *
     * Which argument is the payload is decided twice over, and never guessed:
     *
     *  1. the platform's error shape. Exactly one argument that is not
     *     error-shaped, beside at least one that is, is the success side.
     *  2. what the source reads. Where every argument looks alike — `Pair<A,
     *     string>` — a member read of the carrier that resolves to exactly one
     *     of the arguments names the side this call site takes.
     *
     * Where neither decides, the carrier keeps its own answer and the limit is
     * logged: a coin flip published as a contract is worse than an envelope a
     * reader can see is an envelope.
     *
     * carrick#1841: an argument that is a RESPONSE TABLE (`{ 200: Item }`, every
     * key a status code) says which side it is by its keys, and that decides
     * first: the one table whose keys are all 2xx is the success side, and what
     * a caller receives is the table's body, the member the branch holds, never
     * the table. A success table whose rows state no body (`{ 204: void }`)
     * states no response contract. A carrier built from tables, or found
     * through the call's arguments, that nothing decides states none either:
     * the union is the client's bookkeeping, not a body, so it is not published
     * as written.
     */
    private resultCarrierPayload;
    /**
     * carrick#1841: the branches of a conditional type the compiler left
     * deferred on a type parameter, each with its promise-like peeled, or
     * `undefined` for any other type.
     *
     * Inside a generic function whose result depends on a type parameter it has
     * not fixed (`<Throw extends boolean>(o) => client.get<Table, Errors,
     * Throw>(…)`), the call's result is such a conditional. The compiler states
     * what its branches allow itself: the constraint of the conditional, which
     * is its apparent type. No branch is read off the alias's declaration.
     */
    private deferredConditionalBranches;
    /**
     * carrick#1841: what a call result deferred on a type parameter carries,
     * read through its branches. `undefined` where `type` is no such
     * conditional, or none of its branches is a result carrier, and the type
     * keeps the answer it had.
     *
     * Each branch that is a carrier is read by the carrier read, with the
     * call's own type arguments. Every one of them must name the same payload,
     * by the compiler's type identity, and every other branch must hold that
     * payload as the type of one of its members (the branch a client answers
     * with when it throws on failure holds the body and no error side). Then
     * the payload is the body whichever branch the caller fixes. A carrier
     * branch that states no contract decides for the whole. Branches that
     * disagree state none: the conditional is the client's bookkeeping and is
     * never published as written.
     */
    private deferredConditionalPayload;
    /**
     * The value of a response table's one success row, for a table a carrier
     * holds as itself rather than through its body. `undefined` where the table
     * has more than one success row: their union has no single type here.
     */
    private singleSuccessValue;
    /**
     * carrick#1841: the type arguments a call's result was built from, for a
     * result that no longer carries them. A client whose result type is a
     * conditional alias (`Result<TData, TError, ThrowOnError>`) resolves to its
     * branch at the call, and the branch is an anonymous union with no
     * arguments of its own. The arguments are still stated in two places the
     * compiler keeps:
     *
     *  - the call's own explicit type arguments (`client.get<Table, Errors>(…)`);
     *  - the alias the callee's declared return type was written with, where
     *    the callee is a function wrapping such a call
     *    (`getItem<ThrowOnError>(…)` returning `Result<Table, Errors, ThrowOnError>`).
     *
     * An argument read here names a payload only where a branch of the result
     * holds it (`resultCarrierArguments`), so a type parameter of the callee or
     * a style flag carries nothing.
     */
    private callResultArguments;
    /**
     * The shape test of `resultCarrierPayload`: the carrier `type` is, once a
     * promise-like around it is peeled, and the type arguments a branch of it
     * holds as a member. `undefined` when `type` is not a carrier.
     */
    private resultCarrierArguments;
    /** The member names every branch holds, each with one and the same type. */
    private membersCommonToEveryBranch;
    /**
     * What the source narrows a call's value to, where the type stated at the
     * walk's terminal is a placeholder that leaves a position open
     * (carrick#2714, `as { data: Item[] } | { error: unknown }`): the one type
     * stated for the value later in the function, on its success path
     * (`return json as { data: Item[] }`). A closed statement at the terminal
     * keeps the answer it had.
     */
    private narrowedBodyStatement;
    /**
     * The rule that verifies a call's result as a request library's response
     * object, or undefined (carrick#2714). The result is that object when its
     * awaited type is one a rule recognises. It is that object too when the
     * call states no type arguments and its declared result, with its free
     * type parameters at their defaults (`freeResultAtDefaults`), is a type a
     * rule recognises (`get<T, R = Response<T>>(): Promise<R>`).
     * The compiler then takes the parameter from where the value goes
     * (`return client.post(…)` in a function declared `Promise<void>` types the
     * call `Promise<void>`), and that is the caller's claim about the object,
     * not what the call returns.
     */
    private transportRuleOfCall;
    /**
     * The first rule that matches `type` as a wrapper whose body is a member
     * (`payloadPropertyPath`) the type has, whether or not it reads a payload
     * out of it. A
     * wrapper with no such member (the platform's `Response`) is read by its
     * body reads (`res.json()`), which the def-use walk follows.
     */
    private ruleRecognising;
    /**
     * What a call's declared result is when its free type parameters take
     * their defaults, or undefined. A parameter is free when the declaration's
     * result mentions it and none of its parameters do, so only a type
     * argument, the default or the call's return position can fix it. The
     * result is read through `Promise`, a free parameter (its default), and a
     * conditional alias whose check type is a free parameter tested against
     * exactly that parameter's default (`R extends Default ? Response<T> : R`:
     * the true branch). Anything else is the type as written.
     */
    private freeResultAtDefaults;
    /**
     * The call states the body itself: the rules read a payload out of its
     * result (`client.get<Widget>(url)`). An untyped payload is no payload to
     * the rules (they verify the object and read nothing), and a result its
     * return position chose is no response object to them.
     */
    private callStatesTypedPayload;
    /**
     * The body of a call that returns a response object (carrick#2714): the
     * one type the source states, in the function the call is made in, for the
     * reads of the object's payload member (`res.data as Widget`, `const w:
     * Widget = res.data`, `return res.data` from a function that declares its
     * return type), through a binding or in place. Where no read states one,
     * or two state different types, or the object is handed on whole, the
     * site states no response contract, and the abstain is decided so the
     * capture does not re-read the call and publish the object.
     */
    private transportCallBody;
    /**
     * The reads of a response object's payload member (`path`, e.g. `data`)
     * that take it off the call's value: in place (`(await call).data`),
     * through the name the value is bound to (`res.data`), or as a name
     * destructured out of it (`const { data } = await call`).
     */
    private payloadMemberReads;
    /**
     * The decided abstain of a call whose result carries transport the
     * service's wrapper rules verify and read no payload out of (carrick#1841,
     * carrick#1843): `unknown` with `machinery_envelope` at the root and no
     * anchor. The root reason is what keeps the capture's own locator from
     * re-reading the raw call (`inference_decided_no_contract`,
     * engine/type_compat_v2.rs).
     */
    private transportAbstain;
    /**
     * `Future<T>` -> `T` for a thenable, read off the await protocol rather
     * than a name: a `then` whose first parameter is a callback, whose own
     * first parameter is the value awaiting it yields. `Promise` and
     * `PromiseLike` are peeled by `unwrapPromiseType` before this.
     *
     * The callback is read through `null` and `undefined` (carrick#1877). A
     * `then` of the source's own making declares `(value: T) => void`; the
     * platform's declares `onfulfilled?: ((value: T) => ...) | null`, and that
     * is the `then` a subclass of `Promise` inherits. Read as written, an
     * optional, nullable callback has no call signature, and the subclass was
     * not seen as a thenable at all.
     *
     * A `then` that cannot be called, or whose first parameter is no callback,
     * is no protocol, and the type is returned as it is.
     *
     * The compiler's own awaited type arbitrates. This walk reads the first
     * signature of `then`; the language reads all of them. Where awaiting the
     * type and awaiting what this walk found are not the same thing to the
     * compiler (an overloaded `then` whose first signature is not the one
     * `await` takes), the walk did not read the protocol, and the type is
     * returned as it is rather than published as a guess.
     */
    private unwrapThenableType;
    /** The value `then`'s first signature hands its callback, read until it stops changing. */
    private firstThenValue;
    /**
     * What `await` yields for `type` where the names do not say: `type` is,
     * once `Promise` and `PromiseLike` are peeled, a thenable by the protocol
     * (a subclass of `Promise`, a class with a `then` of its own). `undefined`
     * where the names say it all or `type` is no thenable, so a caller keeps
     * the reading it had (carrick#1877).
     */
    private awaitedBeyondPromise;
    /**
     * The platform's error shape, in full: `name` and `message` strings AND a
     * `stack`, which is what the `Error` interface declares and every subclass
     * of it inherits.
     *
     * `stack` is what makes the test a test. A name and a message alone are a
     * shape a PAYLOAD can have — a contact form declares both — and reading such
     * a payload as the failure side would publish the other argument, which is
     * the concrete-but-wrong answer this whole rule exists to avoid. A union is
     * error-shaped when every member of it is.
     */
    private isErrorShaped;
    /**
     * The member read that takes `identifier` as its RECEIVER — `query` in
     * `query.data`, `envelope` in `envelope.list[0]` — or `undefined` when the
     * identifier names the value itself.
     *
     * A member CALL is not a projection: `res.blob()` yields a body rather than
     * a part of one, and what it returns stays the walk's business. The
     * zero-argument json and text body reads have their own branch and are
     * taken before this is asked.
     */
    private projectionOnReceiver;
    /**
     * The call that CONSUMES this json body read and states what the body is —
     * `parseEnvelope(await response.json())` — or `undefined` when nothing
     * downstream of the read says more about it than the read itself does
     * (carrick#1382).
     *
     * Three conditions, all shapes of the language rather than names:
     *
     *  - the read reaches the call as an ARGUMENT, through the wrappers that do
     *    not change a value (`await`, parentheses, `as`, `satisfies`, `!`). A
     *    cast with no call around it therefore keeps the read as the terminal,
     *    so `(await res.json()) as Entry` is still read off the read itself;
     *  - the call's own result is BOUND — declared into a variable, returned, or
     *    assigned — so a call the source made for its side effect
     *    (`store(await res.json())`) states nothing about the payload;
     *  - that result is an OBJECT shape. A validator answering `boolean` or a
     *    serialiser answering `string` describes what the caller did with the
     *    body, not what the body is, and publishing it would be a
     *    concrete-but-wrong contract where the honest `any` of the read is
     *    merely unresolved;
     *  - the call states its argument (`callStatesItsArgument`): a schema
     *    validates it, or the callee returns it unchanged. A converter's
     *    object result describes what it built, not the body (carrick#2595).
     */
    private statedPayloadAroundBodyRead;
    /**
     * The source keeps this call's result: it initializes a declaration, is
     * returned, is assigned, or is an arrow's expression body. A result that is
     * kept is one the source has a use for; a discarded one is a side effect.
     */
    private callResultIsBound;
    /**
     * A shape a JSON body can be: an object, an array, or a union of them.
     * Top types, primitives, `void` and callables are not.
     */
    private isObjectShape;
    /**
     * Every use of a tracked name inside `expr` reads a member out of the
     * tracked value, so the expression's type describes a PART of the payload.
     * False when the expression uses no tracked name at all, so a caller can
     * read it as "this is a projection" rather than "this is not a use".
     */
    private usesNamesOnlyByProjection;
    private extractBindingFromCall;
    private extractBindingNames;
    private getPrimaryBindingNode;
    /**
     * The zero-argument `.json()` body read on this identifier — `res` in
     * `await res.json()` — or `undefined` (carrick#1017). The JSON body of an
     * HTTP response is read exactly this way whatever produced the response, so
     * the shape is structural, not a framework's name.
     */
    /**
     * The body read inside a chain that starts at `callExpr` (carrick#1749):
     * `request(url).check(ok).mapOk(response => { const data = response as
     * SearchResponse; ... })`.
     *
     * The chain is the run of member calls whose receiver is the call, then
     * that call's result, and so on. A callback passed to one of them whose
     * first parameter the compiler types `unknown` is handed a value nothing
     * has typed yet, which is what a parsed body is; where the source casts
     * that parameter, the cast is what the caller says the body is. The value
     * the chain ends in is what the caller computed from it, and publishing
     * that as the body is the false mismatch this rule exists for.
     *
     * A call that narrows that parameter (a schema validates it, or a function
     * returns it unchanged) states it the same way (carrick#3004).
     *
     * Exactly one such read across the whole chain answers. None, or more than
     * one (a callback on the failure side can be handed an `unknown` too), and
     * `handedUnread` says a callback was handed the body all the same: what it
     * returns is a value built from the body (`.mapOk`, `.then`, `.andThen`,
     * whatever the name), so the value the chain ends in is never the body
     * (carrick#3004). A parameter typed `any` is not read: an unresolved
     * library types every callback that way, success and failure alike, so it
     * says nothing about which one is the body.
     */
    private bodyReadInCallbackChain;
    /**
     * `type`, or a type argument it is instantiated with at any depth, is
     * `any` or `unknown`: `Chain<unknown>`, `Task<Outcome<unknown, E>>`. The
     * value such a carrier hands its callbacks is untyped (carrick#3004).
     */
    private holdsTopTypeArgument;
    /**
     * What a callback states about its first parameter, when the compiler types
     * that parameter `unknown`, or `undefined` when it is not handed one. A
     * statement is a cast whose operand is the parameter itself (`response as
     * T`, `<T>response`), or a call that narrows it (`callStatesItsArgument`:
     * a schema validates it, or a function returns it unchanged). A cast that
     * leaves a position open states nothing about the body and is skipped.
     */
    private statementsOfUnreadParameter;
    /**
     * The zero-argument whole-body read taken in place on the value `callExpr`
     * yields, or `undefined` (carrick#1851): `(await fetch(url)).text()`. The
     * receiver is the call itself, through the wrappers that leave a value as
     * it is (parentheses, `await`, `!`), so it is the same read
     * `bodyReadOnReceiver` finds on a binding of that value. A call that is not
     * awaited first is read the same way: a request that is a promise and reads
     * its own body (`send(url).json()`) yields the body from that read too.
     */
    private bodyReadOnCallValue;
    /**
     * The platform's zero-argument whole-body read that takes `receiver` as its
     * receiver, `res.json()` or `res.text()`, or `undefined` (blueprint row N33:
     * the receiver's declaration decides, not the name). A text read is a body read
     * like a json one (carrick#1842): without it, `return res.text()` left the
     * walk on the response binding and published the transport object. `reads`
     * names the reads asked for.
     */
    private bodyReadOnReceiver;
    private collectDefUseNodes;
    private expressionUsesNames;
    /** `expr` is a use of a tracked name, or contains one. */
    private usesTrackedNames;
    private isIdentifierUsage;
    private isInFunctionScope;
    /**
     * Resolve the awaited type: `Promise<X>` / `PromiseLike<X>` / `Awaited<X>` →
     * `X` at the type level (recursively, so `Awaited<Promise<X>>` → `X`); any
     * non-thenable type passes through UNCHANGED.
     *
     * ts-morph 25.0.1 does not expose the compiler's `getAwaitedType` on its
     * `Type` wrapper (only `AwaitableNode.isAwaited()` on AST nodes, which is
     * unrelated), so we resolve structurally on the symbol/alias name instead of
     * gating on the literal `'Promise'` symbol. This generalizes past `Promise<T>`
     * to `PromiseLike<T>` and the `Awaited<T>` utility type without over-unwrapping:
     * a non-thenable like `AsyncGenerator<T>` is NOT awaitable and is returned
     * unchanged, so the `unwrapAsyncIterableType` step that runs right after still
     * sees (and peels) the iterator wrapper as before.
     */
    private unwrapPromiseType;
    /**
     * `AsyncGenerator<T, …>` / `AsyncIterableIterator<T>` / `AsyncIterator<T>` /
     * `Generator<T, …>` / `IterableIterator<T>` → `T` (the yield type) at the type
     * level; any other type passes through. A GraphQL subscription resolver written
     * as `async function* x(): AsyncGenerator<Order>` carries its contract in the
     * yield position, so the iterator wrapper must be peeled the same way Promise is
     * before structural expansion — otherwise the response contract resolves to the
     * library `AsyncGenerator<…>` machinery instead of the bare `Order`.
     */
    private unwrapAsyncIterableType;
    /** The "leave the type as it is" result every bail-out path shares. */
    private noUnwrap;
    /**
     * Unwrap a type using the agent-generated ExtractionConfig.
     */
    private unwrapTypeWithConfig;
    /**
     * Core unwrapping implementation with ExtractionConfig rules.
     *
     * Requirements:
     * 1. Exact wrapperSymbols match extracts (gated on originModuleGlobs when
     *    the rule carries them — names like `Response` are shared by the DOM,
     *    frameworks, and HTTP clients)
     * 2. machineryIndicators only trigger unwrap if originModuleGlobs also match
     * 3. Handle unions and intersections
     * 4. Support recursive unwrapping with depth limits
     * 5. A rule that matches but extracts nothing never blocks later rules;
     *    only after every rule has run does an origin-verified match with no
     *    recoverable payload collapse to `unknown`
     */
    private unwrapType;
    /**
     * Try to unwrap a type using a single ExtractionRule.
     */
    private tryUnwrapWithRule;
    /**
     * The names `type` goes by, own symbol first (carrick#1843).
     *
     * `type Task<A> = __Task<A>` has the class's symbol and arguments, and the
     * alias's beside them. `type Reply<T> = { ... }` has the anonymous `__type`
     * with no arguments of its own. `type Outcome<A, E> = Done<A, E> |
     * Failed<A, E>` has no symbol of its own at all. In each, the alias and its
     * arguments are what a rule naming `Task`, `Reply` or `Outcome` describes.
     */
    private wrapperReadings;
    /**
     * The type of `type`'s member `name` at `node`, or `undefined` when the type
     * has no such member. The one property read the payload-property rules and
     * the library store's result label (carrick#2818) share.
     */
    private readMemberType;
    /**
     * Extract the payload type from a matched wrapper. Returns null when the
     * rule matched the wrapper but no payload is recoverable from generics or
     * property paths — the caller decides what a payload-less match means
     * (verified machinery collapses to `unknown` after every rule has run;
     * a name-only match leaves the type untouched).
     *
     * `payloadGenericIndex` counts the arguments of `reading`, the name the
     * rule matched (carrick#1843): an alias is free to order its parameters
     * differently from the type it stands for, so the same index into the
     * other list is a different argument.
     */
    private extractPayloadFromWrapper;
    /**
     * True when a producer RESPONSE type IS or CONTAINS framework transport
     * machinery (a fetch/DOM `Response`, a Node `ServerResponse`, a reply object)
     * — the artifact behind carrick#371, where a wrapped handler's literal return
     * envelope `{ response: Response; error?: undefined } | { ...; error: Error }`
     * was captured as the response contract. Machinery is never a comparable
     * contract, so a response path that resolves here abstains (honest `unknown`)
     * rather than emit a concrete-but-false type.
     *
     * DETECTS, exactly (see `typeIsOrContainsMachinery`):
     *   1. the type itself is machinery (`typeIsFrameworkMachinery`);
     *   2. a union/intersection member is machinery (the envelope union);
     *   3. a DIRECT property's type is machinery, ONE level of descent only
     *      (`{ response: Response; error }`).
     *
     * DELIBERATELY NOT DETECTED — listed so this comment never overstates the
     * guarantee (an overstated safety comment is what bit sibling PR #442). Each is
     * a non-regression: it leaves the pre-existing verdict unchanged and never
     * manufactures a new wrong one, tracked as a follow-up:
     *   - machinery nested deeper than one property level (a property whose type
     *     is itself a nested object wrapping the machinery);
     *   - a property typed `Promise<Response>` (the property type is NOT awaited /
     *     Promise-unwrapped before the machinery check);
     *   - an array element type: `Response[]` is not descended to its element;
     *   - `interface X extends Response` declared in USER source — the origin gate
     *     is lib/installed-dependency only, so a user-declared subtype reads as a real
     *     contract, not machinery;
     *   - a function / call-signature return type: the response paths that call
     *     this resolve a handler's RETURN (an envelope/object), never a function
     *     value, so no call-signature descent happens here. The capture-seam
     *     mirror `capture/machinery.ts` DOES descend call signatures — that is
     *     where the wrapper-FUNCTION type the Infer fallback resolves is caught.
     *
     * The origin gate in `typeIsFrameworkMachinery` keeps a user object whose
     * fields merely share a member name from tripping.
     */
    private typeIsOrContainsResponseMachinery;
    private typeIsOrContainsMachinery;
    /**
     * True when `type` itself is an HTTP-machinery type: it structurally carries
     * at least `MACHINERY_INDICATOR_THRESHOLD` of the strongly-discriminating
     * `MACHINERY_MEMBER_INDICATORS`, AND its symbol is declared in a lib
     * (`lib.dom.d.ts`, ...) or installed-dependency origin. Both gates are required —
     * the indicator subset alone essentially never matches a JSON payload, and
     * the origin gate makes certain a user's own local type sharing those member
     * names is never mistaken for framework machinery (the advisor's guard).
     */
    /**
     * Blueprint row N8: true when `type` is framework machinery by where it is
     * declared, never by its name. Either it passes the structural test
     * (`typeIsFrameworkMachinery`: the transport's members, declared by a
     * library or an installed package), or an installed package the store
     * labels a server framework declares it (`frameworkPackages`), which also
     * covers app and router instances that carry no reply members. A
     * service's own `Context` or `Router` is neither.
     */
    private isFrameworkMachinery;
    private typeIsFrameworkMachinery;
    /**
     * True once the type carries at least `MACHINERY_INDICATOR_THRESHOLD` DISTINCT
     * `MACHINERY_MEMBER_INDICATORS` (own + apparent). Deduplicates by name (own and
     * apparent property lists overlap) and early-returns the moment the threshold
     * is reached — the callers only need the boolean, never the full count.
     */
    private hasMachineryIndicatorThreshold;
    /**
     * True when the symbol is declared in a runtime/library origin
     * (`isExternalOrigin`): a TypeScript lib, an installed dependency however
     * the program resolved it, or the runtime declarations Carrick materialises
     * under `.carrick/deno/` — framework/runtime machinery, not user source.
     * Works on a bare checkout: the DOM `Response`/`Request` resolve from the
     * bundled `lib.dom.d.ts` even with no installed dependencies. The program is
     * read per call: the project adds source files lazily and rebuilds it.
     */
    private symbolIsLibOrExternalOrigin;
    /**
     * Check if a type has machinery indicator properties/methods.
     */
    private typeHasMachineryIndicators;
    /**
     * Check if a symbol's declarations originate from modules matching the globs.
     */
    private symbolOriginatesFromModules;
    /**
     * Simple glob matching for module paths.
     * Supports: exact match, a trailing "*" wildcard, and "package/*" patterns.
     *
     * Matches are segment-bounded: the glob names a package (or package
     * subpath) under node_modules, and the match must end at a path-segment
     * boundary — `got` matches `node_modules/got/...` but never
     * `node_modules/got-scraping/...`. This matters because the exact-symbol
     * origin gate routes shared names like `Response` through here.
     */
    private filePathMatchesModuleGlob;
    /**
     * Check if a type string is "useless" for payload purposes.
     */
    private isUselessType;
    /**
     * A type that can be CALLED or constructed is machinery, never a payload
     * (carrick#964).
     *
     * A context-object framework hands the handler one object that both reads the
     * request and sends the response, so that object has a `body` MEMBER whose
     * type is the response sender. Every anchor that reads a member off the
     * handler's parameter, and every locator that lands on such a member, can
     * therefore resolve to a perfectly concrete type that is the wrong side of
     * the exchange — and, being concrete, it sails past the useless-type guard and
     * publishes as an explicit contract.
     *
     * Nothing crosses an HTTP (or queue, or topic) boundary as a function, so the
     * presence of call or construct signatures is a structural, framework-free
     * proof that the resolution landed on machinery. The caller abstains, which
     * leaves the row honestly unresolved instead of confidently wrong.
     */
    private isCallableType;
    private unwrapExpressionNode;
    /**
     * If `node` is a `JSON.stringify(arg)` call, return the (expression-unwrapped)
     * first argument so its type is read instead of the call's `string` result.
     * Otherwise return `node` unchanged. Any non-`JSON.stringify` call, or a
     * `JSON.stringify()` with no argument, is left alone.
     */
    private unwrapJsonStringifyArg;
    private extractExplicitTypeFromAncestor;
    /** The annotation `extractExplicitTypeFromAncestor` prints, as a node. */
    private explicitTypeNodeFromAncestor;
    /**
     * Render an explicit annotation (`as T`, `<T>`, or a typed binding) as
     * fully-structural text.
     *
     * `typeNode.getText()` keeps a named type as its bare identifier
     * (`OrderView`, `Promise<Payment>`). A bare name is fine inside the source
     * project but becomes a dangling reference in the cross-repo `.d.ts` bundle,
     * which carries only alias lines and no source declarations — it resolves to
     * `any` and the comparison reads `unverifiable`. Resolving the annotation to
     * its `Type`, stripping `Promise<…>` at the type level, and expanding the
     * object structurally (shared with `definition-resolver.ts`) lands the real
     * shape (`{ id: string; currency: string }`) in the bundle so the consumer
     * can actually be compared.
     *
     * Falls back to the bare annotation text when the resolved type can't be
     * expanded to a structural form (primitives, library types, unresolvable
     * references), so a non-object annotation behaves exactly as before.
     */
    private expandAnnotationTypeNode;
    /**
     * Producer-side analogue of `expandAnnotationTypeNode` that works from a
     * resolved `Type` rather than a syntactic annotation node.
     *
     * `inferResponseBody`/`inferFunctionReturn` resolve a payload to a named
     * object type (e.g. `Payment`), then render it with `typeText`, which keeps
     * the bare name. In the source-less cross-repo `.d.ts` bundle that name is a
     * dangling `export type <alias> = Payment;` → resolves to `any` →
     * `unverifiable` → `compat = None`. Expanding the resolved object structurally
     * lands the real members (`{ id: string; … }`) in the bundle so the producer
     * can be compared. Mirror of #257's consumer-side fix; keeps the bare text for
     * primitives, library types, and anything the expander leaves by name.
     *
     * `fallback` is the already-computed type text (post Promise/wrapper unwrap),
     * preserved verbatim when expansion does not inline an object.
     */
    private expandResolvedTypeStructural;
    /**
     * Recover a response contract from a handler's own `return` statements when
     * the resolved return type carries none.
     *
     * A file-based route `loader`/`action` almost never annotates its return: the
     * payload goes through a helper — `json(payload)`, `reply(payload, opts)`,
     * `wrap(request, json(payload))` — and the helper's result is transport, not
     * contract. Two ways that collapses today:
     *
     *   * the helper is typed and returns machinery (`Promise<Response>`), so the
     *     carrick#371 guard abstains and the manifest reads `unknown`;
     *   * the helper comes from a package with no installed declaration (the
     *     scanner reads a bare checkout), so it resolves to `any` and the whole
     *     response reads `any`.
     *
     * In both, the contract is the helper's ARGUMENT — every framework that ships
     * such a helper is generic over it. This walks the handler's returned
     * expressions, descends through wrapper calls to the first argument that
     * carries a payload, and unions the surviving branches. It is structural
     * throughout: no helper, framework or package name is matched anywhere, and
     * the recovery only runs where the existing path already had nothing.
     *
     * `statedOnly` is the guard for the unresolvable-callee case. A return type
     * of `any` does not say the callee was a response helper, only that it could
     * not be resolved, and on a bare checkout that is true of most imported
     * callees: `return db.findMany({ where })` would otherwise report the query
     * object as the endpoint's contract, which is a false contract and worse
     * than `unknown`. With `statedOnly` set, only an argument the source
     * annotates (`satisfies` / `as` / `<T>`) counts, because there the developer
     * stated the contract. The machinery case leaves it clear: the callee is
     * known transport, so its argument is the payload whatever its type.
     *
     * Returns `null` (a logged limitation, never a guess) when no returned
     * expression yields an argument type this may read.
     */
    private recoverPayloadFromReturnStatements;
    /**
     * The response payload carried by a set of response-carrying expressions —
     * a handler's return expressions, or the single expression a producer
     * response locator resolved to (carrick#1017).
     *
     * Each expression is first split into its BRANCHES, so a route that answers
     * `found ? json(entry) : json(problem, { status: 404 })` contributes both
     * sides: the payload rule reads each one, the error branch drops out on its
     * stated status, and what survives is joined as a union exactly as several
     * return statements are.
     */
    private recoverPayloadFromResponseExpressions;
    /** How one payload node reads: its stated annotation, else its resolved type. */
    private payloadCandidateOf;
    /** A payload whose contract a type node in the source states. */
    private statedPayloadCandidate;
    /**
     * Join the payloads of several response branches into one contract, as
     * several return statements are joined: an empty object drops beside a
     * branch with members, equal texts dedupe, and the rest is a union.
     */
    private joinPayloadCandidates;
    /**
     * True when a handler's awaited return, or a member of its union, is a
     * serialised-body envelope: an object with a numeric `statusCode` or
     * `status` member and a `body` member typed `string`. Either member may be
     * optional, as a platform's result type declares them.
     *
     * HTTP vocabulary only, the class `RESPONSE_INIT_MEMBER_NAMES` is. A `body`
     * typed as an object is not an envelope: a runtime that serialises the
     * handler's return value sends that object as the payload, so the shape is
     * ambiguous and keeps the ordinary reading.
     */
    private typeIsSerialisedBodyEnvelope;
    /** A member's type with `undefined` and `null` removed, or `undefined` if absent. */
    private envelopeMemberType;
    /**
     * True when a handler's return is a serialised-body envelope AND the source
     * shows the body is serialised, so the envelope reading applies:
     *  - a returned branch builds an envelope whose `body` is `JSON.stringify`
     *    of a value, inline, through a `const`, or in a builder the reading can
     *    follow (`branchShowsSerialisation`); or
     *  - the envelope type is declared outside the repository (a platform's
     *    result type) with every member optional. Nothing else returns such a
     *    type, so a value of it returned as-is is still transport. The repo's
     *    own all-optional types (`Partial<Article>`) engage only on evidence.
     *
     * A domain object that merely has `status` and `body` members (an article,
     * a message) shows neither and keeps the ordinary reading.
     */
    private serialisedBodyEnvelopeEngages;
    /**
     * True when a returned branch builds an envelope from a serialised value:
     * a literal whose `body` is `JSON.stringify(...)`, a call to a builder the
     * reading can follow, or a call handing on one such envelope.
     */
    private branchShowsSerialisation;
    /**
     * The arguments a call could hand back as its result: every argument but
     * an object literal, which is the call's input (`create({ body:
     * JSON.stringify(doc) })` stores a document, it does not respond with it),
     * and but a function, whose returns are read as the call's callbacks.
     */
    private handedOnArguments;
    /**
     * The returned branches of an envelope handler: each returned expression,
     * conditional sides split. A returned call whose callbacks return values
     * (`settle().then(ok, fail)`) is carried by those returns, plus every
     * envelope it is handed directly (`withFallback(() => a, b)` can answer
     * `b`); a call whose callbacks return nothing stays a branch of its own.
     */
    private envelopeBranches;
    /**
     * The `function_return` answer for a handler that returns a serialised-body
     * envelope: the union of the values its success branches serialised.
     *
     * Each returned branch is read by `readEnvelopeBranch`. Error and redirect
     * branches contribute nothing (carrick#1161). If any branch cannot be read,
     * the answer is `unknown` with the decided reason `serialised_body_unread`
     * and one sentence per cause: the branches that did read are part of the
     * contract, and neither the envelope nor its `string` body is any of it.
     */
    private inferSerialisedBodyEnvelope;
    /**
     * carrick#2674: the locator names a request call's options object literal
     * and the member that carries the call's body. The call sends no request
     * body when the literal can write no such member: no spread, no computed
     * key, and no member of that name in any form (assignment, shorthand,
     * method, accessor, quoted key), nor any other body-named member
     * (`BODY_MEMBER_NAMES`, `REQUEST_BODY_PARTS`). Anything else is not decided here, so the
     * answer is null. `configArgumentBody` stays the reader of the member
     * itself; the checks above it are this kind's own.
     */
    private inferRequestConfig;
    /** An `unknown` answer the inferrer decided, carrying its reason at the root. */
    private decidedAbstain;
    /**
     * What one returned branch of an envelope handler serialised:
     *  - an envelope object literal: the argument of the `JSON.stringify` that
     *    produced its `body`, inline or through a `const` in the same function;
     *    its status member decides the branch;
     *  - a call to a response builder this repository declares
     *    (`envelopeBuilderOf`): the argument the builder serialises; the
     *    argument or literal its status member takes decides the branch;
     *  - any other call handing on exactly one envelope argument (never an
     *    object literal, which is the call's input): that
     *    envelope is read in its place. Its own stated status decides the
     *    branch; only when it states none does a status the outer call's
     *    arguments state decide (`log(ctx, 503, "code", prebuilt)`).
     * Anything else, and a payload holding `any` or `unknown`, is unread, with
     * its cause.
     */
    private readEnvelopeBranch;
    /** The status a builder call sends: its status argument, or the builder's literal. */
    private builderCallStatus;
    /**
     * The status an envelope expression states itself: a literal's status
     * member, or what a followable builder call sends. `'undecided'` for
     * anything else (a variable holding an envelope states nothing here).
     */
    private envelopeOwnStatus;
    /** The status an envelope literal's `statusCode` or `status` member states. */
    private envelopeLiteralStatus;
    /** The status a value states, classified as a send's status argument is. */
    private statusOfValue;
    /**
     * The value node an object literal gives a member: a property assignment's
     * initializer, or a shorthand's own name.
     */
    private envelopeMemberValue;
    /**
     * The value an envelope literal's `body` serialises: the argument of a
     * `JSON.stringify` written there, or written as the initializer of a
     * `const` that `scope` declares and `body` names. `undefined` when `body`
     * is built any other way.
     */
    private serialisedBodyArgument;
    /**
     * The initializer of the `const` an identifier (or a shorthand member's
     * name) refers to, when `scope` itself declares it.
     */
    private constInitializerInScope;
    /**
     * The symbol of the value a node names: a shorthand member's value symbol
     * (its own symbol is the property), else the identifier's symbol.
     */
    private valueSymbolOf;
    /** The index of the `params` entry a node names, or `undefined`. */
    private parameterIndexOf;
    /**
     * How a call's callee builds an envelope, when the callee is a function this
     * repository declares (not lib, not an installed package) and every return
     * it makes is an envelope literal:
     *  - returns whose status member is a literal 4xx or 5xx are its failure
     *    path and are ignored; a builder with nothing else is `'failure'`, so a
     *    call to it is an error branch;
     *  - every other return must serialise the same parameter into `body` and
     *    take its status from the same parameter, or state the same literal
     *    success or redirect status.
     * `undefined` when the callee is anything else.
     */
    private envelopeBuilderOf;
    /**
     * The single function a callee symbol declares with a body: a function
     * declaration, or a variable initialised with an arrow or function
     * expression.
     */
    private builderFunctionOf;
    /**
     * The payload a builder call serialises. The source states it, and the
     * answer is explicit, when the argument carries `satisfies`/`as`, when the
     * builder's parameter is one of its type parameters and the call passes
     * type arguments (`reply<AddNoteResult>(201, x)`), or when the parameter is
     * declared with a type that says something. Otherwise the argument's own
     * type is read.
     */
    private builderPayloadCandidate;
    /**
     * True when a declared parameter type does not say what is sent: a top
     * type, the `object` keyword, an object type that names no member (`{}`,
     * `Record<string, unknown>`), a union that admits both primitives and
     * objects (a JSON-value type such as `string | number | Json[] | { [k:
     * string]: Json }`), or a type that mentions a type parameter. A builder
     * that takes "any value" states nothing about the payload a call hands it,
     * so the argument's own type is read instead.
     */
    private typeNodeStatesNothing;
    /** A serialised value's candidate, unread when it holds a top type. */
    private envelopePayloadCandidate;
    /**
     * A payload candidate, or unread when its printed type is or holds `any`
     * or `unknown` anywhere (`{ [x: string]: any }`, `{ data: any }`). A
     * contract with a hole in it is not published on this path: the capture
     * would refuse the text and re-read the line, which can publish the
     * envelope.
     */
    private checkedEnvelopeCandidate;
    /**
     * The request field each member of a joined union answers
     * (`response-modes.ts`). Case texts are joined by the published union's
     * rule. A reading that fails is no reading: it must never cost the union.
     */
    private responseModesOf;
    /**
     * The expressions a function returns, excluding returns belonging to nested
     * functions (a `.map(x => …)` callback returns a row, not the response).
     * A concise arrow body is itself the returned expression.
     */
    private returnedExpressions;
    /**
     * The expressions that can carry this handler's response: its own returned
     * expressions, plus the returned expressions of any function the handler
     * hands DIRECTLY to a returned call (carrick#707).
     *
     * `return settle(...).then(ok => serialise(body), err => serialise(problem))`
     * returns a call whose result is whatever the callbacks produce, so the
     * callbacks' returns are this handler's returns as surely as its own. That
     * shape is how a result-type API, a promise combinator or a matcher is
     * written, and on a bare checkout the callee resolves to nothing, so the
     * outer call's type says nothing at all.
     *
     * Only ONE level, and only from the returned call's own arguments. A
     * callback nested deeper is inside an expression the handler computes, not
     * an expression it returns — `return rows.map(r => …)` returns a row list,
     * and reading the callback there would report a row as the response. Each
     * flattened expression still has to pass the payload rule below, which no
     * bare object literal does.
     */
    private responseReturnedExpressions;
    /**
     * The payload argument of a returned response-helper call, or `undefined`.
     *
     * Walks the call's arguments in source order: the first whose type reads as a
     * contract wins; an argument that is itself a call is descended into, which is
     * what recovers `wrap(request, json(payload))` — the transport wrapper's own
     * first argument is the request, which never reads as a contract.
     *
     * A call whose sibling options object states a >= 400 status is an error
     * branch and contributes nothing: the response contract of an endpoint is the
     * shape it returns when it succeeds.
     *
     * `statedOnly` narrows what counts as a payload to an argument the source
     * annotates; see `recoverPayloadFromReturnStatements`.
     */
    private responseHelperPayloadNode;
    /**
     * True when a call's callee is a function this repository declares whose
     * every returned branch is a send stating an error or redirect status, or a
     * call to another such function (carrick#3523). `return failure(result)` is
     * then an error branch, as `json({ error }, { status: 400 })` written inline
     * is, and the value handed to it is never the success body. A callee that
     * returns anything else (a success send, a status the source does not fix,
     * a non-send) keeps the ordinary argument reading.
     */
    private callsFailureHelper;
    /**
     * True when a node's type reads as a response payload.
     *
     * Object-shaped only (an object literal, a named interface, an array of
     * either, or a union containing one). A bare primitive argument is a
     * redirect location, a status code or a body string — `redirectTo("/next")`
     * must never report `"/next"` as the endpoint's contract. A stated
     * `satisfies`/`as` annotation counts even when its declaration is missing:
     * the source says what the contract is.
     *
     * Under `statedOnly` the annotation is the ONLY thing that counts, so an
     * unresolvable callee's arguments never become a contract by accident.
     *
     * `asSent` judges the object in the form it is sent in, which is what an
     * ARGUMENT handed to a sender is asked; see `typeIsObjectShaped`.
     */
    private nodeCarriesPayloadContract;
    /**
     * Object, array-of-object, or a union/intersection containing one.
     *
     * With `asSent` the object is judged in the form it is SENT in
     * (carrick#1163): a value that declares `toJSON()` travels as what that
     * returns. A `URL` or a `Date` is therefore the string it serialises to,
     * the bare primitive this rule already refuses, and `redirect(new URL(path,
     * base))` hands over a location exactly as `redirect("/next")` does
     * (carrick#807). A value whose JSON form is itself an object is a body like
     * any other.
     */
    private typeIsObjectShaped;
    /**
     * The `satisfies X` / `as X` / `<X>` annotation node on an expression, when
     * the source states its type. `satisfies` is the shape a route uses to claim
     * a shared response contract without widening the literal.
     */
    private statedTypeNodeOf;
    /**
     * True when an object literal argument is response INIT rather than a body:
     * every property it declares is one the standard `ResponseInit` declares
     * (`status`, `statusText`, `headers`), and it states at least one of them as
     * init really does — a status in the HTTP range, or headers.
     *
     * The status does NOT have to be a literal code. A route that carries its
     * outcome in a value writes `new Response(body, { status: result.status })`,
     * where the source fixes no code and `statedStatusCodes` answers
     * `'variable'` — status-shaped, just not pinned. Requiring a literal there
     * left the whole init object reading as a body, so an endpoint whose payload
     * this layer does not publish (a string body) published `{ status: number }`
     * as its response contract instead: a wrong contract, served, where an
     * abstention was the honest answer.
     *
     * A payload that merely has a `status` member of its own (`{ status: "ok",
     * service: "ledger" }`) declares members init does not, or states `status` as
     * something other than an HTTP code, so it stays a payload.
     */
    private statesResponseInit;
    /**
     * What a response send states about its HTTP status (carrick#1161).
     *
     * Read in order, and the first that states anything decides:
     *
     *  1. an argument past the body that carries a status: a numeric literal
     *     (`send(body, 404)`), an options object (`{ status: 401 }`), or any
     *     expression whose TYPE is a status literal or a union of them
     *     (`let code: 400 | 500`). A numeric argument whose type is a plain
     *     `number` states a status the source does not fix, which is
     *     `variable`: the branch fails open into the union and is logged.
     *  2. the send call's own RESULT type, when a member of it is typed as a
     *     status literal. A framework that types its sends records the status
     *     there, and a redirect helper's default `302` is only visible there.
     *     A result whose status member spans success and error codes (the
     *     default of a typed `json(body, status?)`) says nothing either way.
     *
     * A status in the FIRST argument is a field of the body and is never read.
     * No method or framework name is consulted anywhere.
     */
    private responseSiteStatus;
    /**
     * The status an earlier call of a send's own chain states: the receiver
     * calls `call` is invoked on (`a.status(401).json(body)` has `a.status(401)`),
     * innermost last. Each receiver call's first argument is read by the rule
     * an argument past a body is, so a numeric literal or a literal-typed value
     * counts and a plain `number` is `variable`. `undefined` when no receiver
     * call states a status. No method name is consulted: a receiver call that
     * is handed no status-shaped value says nothing.
     */
    private chainStatus;
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
    private statedStatusCodes;
    /**
     * Status codes a send call's result type records: the members (of the
     * result, or of each part of an intersection result) typed as a status
     * literal or a union of them. `undefined` when none is.
     */
    private sendResultStatusCodes;
    /**
     * Peel the wrappers that do not change an expression's payload: parentheses
     * and `await`. Unlike `unwrapExpressionNode` this KEEPS `as`/`satisfies`,
     * because the annotation is exactly what the recovery wants to read.
     */
    private peelTransparentExpression;
    /**
     * The branches a single response expression can take — one entry for a plain
     * expression, one per side of a conditional (carrick#1017).
     *
     * `return entry ? json(entry) : json({ error }, { status: 404 })` is one
     * return statement holding two responses. Read whole, its type is the
     * transport wrapper both sides share and the payload is lost; read per
     * branch, each side goes through the ordinary payload rule, which is also
     * what drops the error branch on its stated status. Nested conditionals are
     * followed to a small depth; anything else is returned as itself.
     */
    private expandResponseBranches;
    /**
     * The symbol an anchor is named from: the type's own symbol, unless that
     * symbol's name is synthetic (`__type`, `__object`, ...), in which case the
     * alias symbol (carrick#2148).
     *
     * `type Widget = { id: string }` and an instantiated `Page<Order>` carry
     * the anonymous `__type` as their own symbol and the alias as their alias
     * symbol. Reading the own symbol first and stopping because it is truthy
     * left every object-literal alias without an anchor, while the same shape
     * declared as an interface had one. The own symbol still wins whenever it
     * has a real name, so every type anchored before keeps its anchor.
     */
    private anchorSymbolOf;
    /**
     * The deterministic source symbol of a resolved type (`Payment` for a payload
     * typed `Payment`), or `undefined` when there is no single user-defined
     * symbol to anchor on. This is the same `getSymbol() || getAliasSymbol()` name
     * the socket anchor already derives (`socket_io.rs`), filtered through
     * `BUILTIN_ANCHOR_SYMBOLS` so TS/lib globals (`Promise`, `Array`, `Date`,
     * primitives, …) never become an anchor. Used to populate
     * `primary_type_symbol` so the manifest anchor no longer depends on the LLM.
     */
    private primaryTypeSymbol;
    /**
     * Declaration file (absolute path) of the anchor symbol
     * `primaryTypeSymbol` reports for this type, or `undefined` when the type
     * has no user-facing anchor or no source declaration. Every path that
     * reports the symbol reports this beside it (carrick#1819): it is where a
     * reader finds the type an anchor names. The scanner's pub/sub two-anchor
     * arbitration (carrick#413) also uses it to re-aim a demoted explicit
     * bundle request: the bundler resolves a `SymbolRequest` only against
     * declarations IN its `source_file`, so the request must point at the file
     * that actually declares the tsc-witnessed payload type.
     *
     * The declarations read are those of the symbol the anchor is named from
     * (`anchorSymbolOf`: the type's own symbol, or its alias's), so a name
     * imported through a barrel reports the file that declares it, and a name
     * two files declare reports the one this type resolves to. A declaration in
     * an installed package or a TypeScript lib is reported as it is found, as
     * an absolute path; what a reader is shown for it is the scanner's call.
     *
     * Only declaration kinds the bundler's `validateSymbols` can resolve
     * (interface, type alias, class, enum, function, variable) count. A
     * symbol's declaration list can also contain re-export machinery — a
     * barrel's `ExportSpecifier` (`export { Foo } from './foo'`) points at a
     * file that does not DECLARE the type, and a request re-aimed there would
     * fail validation, turning a resolvable explicit type into `unknown`.
     * With no declaring node, no source is reported and the arbitration
     * fails closed to the explicit anchor.
     */
    private primaryTypeSymbolSource;
    /**
     * The anchor a `satisfies X` / `as X` / `<X>` annotation states IN SOURCE:
     * the name written at the annotation, where its type is declared, and the
     * array levels wrapped around it (`satisfies Order[]` → `Order`, depth 1).
     *
     * carrick#768: reading the anchor off the RESOLVED type works for an
     * interface, whose type carries its own symbol, and fails for
     *
     *     export type OrderBody = Inferred<typeof OrderSchema>;
     *
     * because the alias resolves to an instantiated type TypeScript keeps no
     * alias symbol on, so `getSymbol()` answers the synthetic `__type`. The
     * route then published a correct shape with no name, and
     * `primary_type_symbol` is precisely how a reader gets from a route to the
     * type its consumer imports. The annotation names it either way, so read
     * the name from the AST and confirm it against the declaration it resolves
     * to. This is the fallback for a resolved type that anchored nothing, never
     * an override of one that did.
     *
     * Four things are rejected, so a name only anchors when it is genuinely the
     * contract a consumer would import:
     *  - a generic instantiation (`satisfies Envelope<Order>`) names the
     *    WRAPPER, and a bare `Envelope` is not the payload;
     *  - a name resolving to no type declaration — an import of a value, a
     *    re-export the checker cannot follow — is not importable as a type;
     *  - a declaration in the TypeScript default library, and every
     *    `BUILTIN_ANCHOR_SYMBOLS` name, which describe machinery not contracts;
     *  - an inline annotation (`satisfies { id: string }`), which names nothing.
     */
    private writtenAnchorOf;
    /**
     * Peel array levels off a resolved type: `TimelineEvent[]` → element
     * `TimelineEvent`, depth 1. An array type's own symbol is `Array` (builtin,
     * filtered), so without this a `T[]` payload has NO anchor and — worse — an
     * explicit anchor bundled for the same alias silently drops the array-ness
     * (#306: array-vs-scalar scored compatible). The element drives the anchor
     * symbol; the depth is reported on the `InferredType` so the bundler's
     * existing `array_depth` wrap (#248) can restore the `[]` levels on the
     * explicit bundle. Depth is capped at the bundler's sane ceiling; a deeper
     * type is treated as depth 0 rather than a runaway loop.
     */
    private unwrapArrayLevels;
    /**
     * Resolve the target node using the best available locator:
     * 1. If span_start + span_end present → findNodeAtSpan (SWC byte offsets)
     * 2. If expression_text present → findNodeByText (Gemini text + line)
     * 3. Otherwise → undefined
     */
    /**
     * carrick#695: the type of the RECEIVER of a member call, plus the two facts
     * a role decision needs about it — which package declares that type, and what
     * the invoked member returns.
     *
     * Nothing here classifies. A member call with a route-shaped literal
     * (`x.verb("/lit", arg)`) is a route registration or a request depending on
     * what `x` IS, and that is the only question this answers. The Rust driver
     * maps the declaring package onto the detected framework / data-fetcher
     * lists; the compiler layer names no library and applies no shape rule.
     *
     * A receiver that does not resolve reports its top type (`any`) with no
     * package, so the caller can tell "unresolved" from "declared here": on a
     * checkout with no installed dependencies EVERY dependency-typed receiver is
     * `any`, and treating that as workspace-owned would invent a role.
     */
    private inferReceiverType;
    /**
     * The npm package name that declares a type. `undefined` when the type has
     * no declaration to read (a top type, a primitive, an anonymous object
     * literal) or when nothing owns its declaration but the workspace itself.
     * The declaration is the attributed one (carrick#2031), and the package is
     * named by the module graph first, then by the path (`installedPackageOf`).
     */
    private declaringPackageOf;
    /**
     * The awaited return type of the member invoked on the receiver. Reported as
     * a fact next to the receiver; a caller that classified on it alone would be
     * back to a shape rule (any async helper taking a path would match).
     */
    private memberReturnTypeOf;
    /**
     * A type's own name when it has one, else its printed form.
     *
     * The printed form of a type whose declaring module the use site does not
     * import is `import("/abs/path/to/module").Name` — an absolute path that
     * would be persisted and rendered. The name alone is both stable and the
     * part a reader (or a package-list lookup) uses; anonymous types, top types
     * and primitives have no name and keep their printed text.
     */
    private namedTypeLabel;
    /**
     * A type's own name, or `an unnamed branch` when it has none.
     *
     * Unlike `namedTypeLabel` this never falls back to the printed form, because
     * its output reaches a reader as a provenance `detail`, and the printed form
     * of a type whose declaring module the use site does not import is
     * `import("/abs/path").Name`. The bar for a detail is "one scrubbed sentence,
     * never an absolute path", so an anonymous branch is described rather than
     * printed.
     */
    private pathFreeTypeLabel;
    private resolveTargetNode;
    /**
     * Resolve to a CallExpression using the best available locator.
     */
    private resolveTargetCallExpression;
    /**
     * Resolve to a containing function using the best available locator.
     */
    private resolveContainingFunction;
    /**
     * Find the function whose declaration starts at (or within a couple of lines
     * of) the given line. Used for signature inference, where the only locator is
     * the function's start line as recorded by the scanner. Ties break toward the
     * innermost (smallest) function. Returns undefined if nothing is close enough,
     * to avoid binding to an unrelated function.
     *
     * The tolerance is one-directional in effect (carrick#766). Looking BACK is
     * free: a function starting before the anchor is one the anchor sits inside
     * or just after, which is what an anchor recorded on a signature or body line
     * means. Looking FORWARD is how the anchor reaches a handler that starts a
     * line or two into the registration or binding it names — and it is also how,
     * unguarded, an anchor landing on a statement that declares no function
     * (`export { action, loader };`) was answered by the NEXT declaration in the
     * file. That answer is silent: nothing about it says it came from a
     * neighbouring helper.
     *
     * So a forward candidate must be one the anchor line's own statement leads
     * to: rejected when a statement begins at or after the anchor line and does
     * not contain it. Nothing but trivia may sit between the anchor and the
     * function it is taken to name. With no candidate left the caller abstains,
     * which is the honest answer and the one a consumer check can act on.
     */
    private findFunctionByLine;
    /**
     * Walk up from a node to find its innermost containing function.
     */
    private findContainingFunctionForNode;
    /**
     * Follow a LINE-ONLY route-registration anchor to its handler function. The
     * scanner sends a bare line number (no span/text) for a named-handler
     * registration whose handler is declared away from the registration line
     * (`InferLocator::Line`). Find a registration node — a call or object literal
     * — that STARTS on that line and whose handler resolves, and return it.
     * Structural (call with a function-typed arg, or handler-shaped object-literal
     * property), no framework name-lists. Used only when the primary locator did
     * not resolve a node, so no existing path changes.
     */
    private handlerAtLine;
    /**
     * The route registration on `line` together with the handler it references.
     *
     * `handlerAtLine` returns only the handler, but the route's DECLARED contract
     * (a validation-schema object passed alongside the handler) lives on the
     * registration node itself, so the schema anchors need both halves.
     */
    private registrationAtLine;
    /**
     * Follow a route-registration locator ONE indirection to the handler function.
     *
     * The scanner points the infer request at the registration site, but the
     * type-bearing expression (the handler's return, or its first request read)
     * lives inside the handler function, which the registration only *references*.
     * This resolves that reference to the handler's `FunctionLike` node, purely
     * structurally, in three shapes:
     *
     *  1. INLINE — the node is (or is inside) a call whose arguments include an
     *     arrow/function expression: `app.post('/x', async (c) => { … })`. The
     *     inline function IS the handler.
     *  2. OBJECT-LITERAL HANDLER PROPERTY — the node is (or is inside) an object
     *     literal with a `handler`-like property whose value is either an inline
     *     function or an identifier bound to a function:
     *     `{ method, path, handler: healthCheckHandler }`. Resolve that value.
     *  3. IDENTIFIER ARGUMENT — the node is (or is inside) a call whose 2nd+
     *     argument is an identifier bound to a function:
     *     `app.get('/x', healthCheckHandler)`. Resolve that identifier.
     *
     * No framework name, method name, or property name beyond the generic
     * `handler`-shaped key is hardcoded — the match is on node SHAPE (call with a
     * function-typed arg / object literal with a function-valued property), which
     * is what "route registration" structurally is across Express, Fastify, Hono,
     * Koa, a hand-rolled registry array, etc. Returns undefined when the locator
     * is not a registration shape, so every existing non-registration path is
     * untouched.
     */
    private resolveRegisteredHandler;
    /**
     * Request-path gate for handler-following: return the handler function ONLY
     * when the located node is ITSELF (after expression-unwrap) the registration
     * — a call whose arguments include a handler function, or a route-registry
     * object literal with a handler property. Unlike `resolveRegisteredHandler`,
     * this does NOT walk up to an ancestor call, so a consumer's inner
     * `JSON.stringify(payload)` (nested inside a `fetch(...)` that may carry other
     * callbacks) is never mistaken for a registration and the existing consumer
     * request paths stay exactly as they were.
     */
    private registrationHandlerAt;
    /**
     * The handler function referenced by a route-registration call: the LAST
     * argument that is an inline function, or an identifier bound to a function
     * (carrick#2472 P3-2b). The functions before it are middleware
     * (`route(path).post(auth, audit, handler)`), which runs first and hands on;
     * reading the first function read a middleware as the route. The path
     * literal is not function-valued, so it is skipped naturally. No callee-name
     * check: a call whose argument resolves to a function is structurally a
     * registration regardless of the framework.
     */
    private handlerFromRegistrationCall;
    /**
     * The handler function carried by a route-registry object literal. Matches a
     * property whose name is `handler`-shaped (case-insensitive `handler`) and
     * whose value is an inline function or an identifier bound to a function.
     */
    private handlerFromObjectLiteral;
    /**
     * Resolve a node to a handler `FunctionLike`: an inline arrow/function
     * expression is returned directly; an identifier is followed to its binding
     * declaration (via the compiler's definition nodes) and returned when that
     * declaration is — or initializes to — a function. Anything else (a path
     * literal, an object, a non-function binding) yields undefined.
     * `followWrapperCalls` false reads a binding to a wrapper call as no
     * function, so a name the wrapper walk meets is never walked again.
     */
    private asHandlerFunction;
    /**
     * Extract a `FunctionLike` from a declaration node the compiler resolved an
     * identifier to: a function declaration is itself the function; a variable
     * declaration / binding whose initializer is an inline function yields that
     * function (`const h = async () => { … }`), and one whose initializer is a
     * wrapper call yields the function the call wraps
     * (`const h = wrap<Contract>(async (req, res) => { … })`, carrick#2472
     * P3-2b; `functionWrappedBy`). Import/re-export shims are walked by
     * ts-morph's `getDefinitionNodes`, so no manual import chasing is needed.
     */
    private functionFromDeclaration;
    /**
     * The function a call wraps, for a name bound to that call and used as a
     * handler (carrick#2472 P3-2b): the call's one argument that is a function
     * literal, a name bound to one, or another such call, a few calls deep.
     *
     * Two conditions keep this to wrappers. The call's own value has to be
     * callable, because the name is used as a function: `rows.map((row) => …)`
     * is handed a function and yields a list. And exactly one argument may be a
     * function: with two there is no saying which of them is wrapped.
     */
    private functionWrappedBy;
    /**
     * True when `handler` reads nothing from the incoming message (carrick#827):
     * every use of one of its parameters, named or destructured, is the receiver
     * of a call chain whose value the handler discards or returns
     * (`reply.json(result)`, `return c.status(201).json(result)`). That is a
     * send. A returned chain must end in a call that takes an argument, so a
     * read handed straight back (`return request.json()`) is not one.
     *
     * Anything else may be a read of the body and makes this false: a property
     * read, a call whose value is used, a parameter handed whole to another
     * function, or a call in the chain handed an argument that can receive what
     * the parameter gives (`argumentCanReceive`: a callback, an object with a
     * method), as in `request.pipe(dest)`, `request.on('data', cb)` and
     * `request.json().then(cb)`. No method or property name is consulted. A
     * handler with no parameter, or one that uses `this`, proves nothing: it may
     * read the message from `this` or a closure.
     */
    private handlerReadsNothingFromItsParameters;
    /**
     * True unless `argument` is proven to be data that cannot receive anything
     * from the call it is handed to (carrick#827). Data is a primitive or literal,
     * `null`/`undefined`, or an object with no call or construct signature and
     * no callable member; an array of such, or a union all of whose members are.
     * A callback, an object with a method (a stream, an emitter), or a type the
     * compiler cannot state (`any`, `unknown`, a type parameter) can receive.
     * Sibling of `callResultIsPlainData`, which reads a call's result and so
     * rejects primitives and empty objects; an argument of either is data.
     */
    private argumentCanReceive;
    /**
     * Scan a handler function body for the FIRST request-body read that carries a
     * type, and return that type's structural text — or null when the body holds
     * no typed request read.
     *
     * "Typed request read" is matched on SHAPE, never on a callee/method name:
     *
     *  A. a CALL that carries an explicit type argument — `c.req.json<T>()`,
     *     `parseBody<T>(req)`; the first type argument is `T`; or
     *  B. an expression with an explicit type annotation or cast in its immediate
     *     binding context — `const b: T = …`, `req.body as T`.
     *
     * The body is walked in source order (`forEachDescendant` is a pre-order
     * traversal), so the FIRST such read wins, mirroring "the request type lives
     * at the first body read" without assuming which framework produced it.
     * Returns null (not a spurious type) for a genuinely payload-less handler.
     */
    private inferRequestReadFromHandler;
    /**
     * The call in a handler that reads the request's body off the platform
     * request the handler was handed, or `undefined` (carrick#807).
     *
     * Read by shape, with no method name consulted:
     *
     *  - a call that takes nothing, on a member of a value whose type is request
     *    machinery (`typeIsFrameworkMachinery`: declared by the platform or an
     *    installed library, and carrying its body readers);
     *  - that value is rooted at one of the handler's OWN parameters, named or
     *    destructured. A response read off an outbound call inside the handler
     *    (`(await upstream.json()) as Rate`) has the same shape one variable
     *    away, and is the opposite side of a different exchange;
     *  - and the call's result, awaited, is `any` or `unknown`: the platform's
     *    untyped parse. A read that states its own type (`formData()`, a typed
     *    `json<T>()`) is not this shape and is left to the readers that
     *    already handle it.
     *
     * The first such call in source order: a body is read once.
     */
    private platformBodyReadIn;
    /**
     * The platform body read whose result `located` is a MEMBER of, or
     * `undefined` (carrick#2057).
     *
     * A member is a property or element access off a binding, or a name
     * destructured out of one. The binding must be initialised by the body read
     * `platformBodyReadIn` finds in the function that declares it, with only
     * transparent wrappers between the two (`await`, parentheses, `!`, a cast,
     * `satisfies`). The binding itself is not a member, and neither is a member
     * of anything else (a query object, a parameter, an outbound response).
     */
    private parsedBodyReadHolding;
    /**
     * The request contract the source states AT a body read, or null when it
     * states none there (carrick#807).
     *
     * Three statements count, each of them on the read itself:
     *
     *  - a cast on it: `(await request.json()) as NewWidget`;
     *  - the annotation of the binding it initialises:
     *    `const input: NewWidget = await request.json()`;
     *  - a type the handler states for the read further on
     *    (`statedInFunction`): a schema it is handed to, the annotation of a
     *    name it is assigned to, a pass-through function, a cast of the whole
     *    value.
     *
     * The first two are `typeNodeStatedAtRead`, the reading a consumer's
     * response read already gets. An annotation further out is not one of them:
     * `const saved: Saved = await save(await request.json())` types what `save`
     * returned, and reading it as the body would publish the wrong side of the
     * handler. Nor is a placeholder (`as unknown`, `Record<string, unknown>`).
     */
    private requestStatedAtBodyRead;
    /**
     * Resolve a type-annotation/type-argument node to fully-structural text,
     * dropping any `Promise<…>` wrapper (`c.req.json<T>()` returns `Promise<T>`,
     * but the annotation node is `T` directly; the guard is harmless either way).
     * Returns null when the node resolves to a useless/library type that carries
     * no member shape, so the caller keeps scanning rather than locking onto
     * `any`/`unknown`.
     */
    private structuralTextFromTypeNode;
    /**
     * Same reduction as `structuralTextFromTypeNode`, but starting from a resolved
     * `Type` rather than a syntax node. `at` only supplies the scope the type is
     * rendered in.
     */
    private structuralTextFromType;
    /**
     * The route registration a request's locator points AT, or undefined when it
     * points at anything else.
     *
     * Deliberately strict: the located node must itself be the registration (a
     * call registering a handler, or a route-registry object literal), never an
     * ancestor of it, so an expression inside a handler is not mistaken for the
     * route it belongs to. A locator that resolves nothing falls back to the
     * registration on the request's line.
     */
    private routeRegistrationForRequest;
    /**
     * Anchor (b) on the response side: the success-status contract declared in the
     * registration's schema, as an `InferredType` ready to return. Null when the
     * registration declares no resolvable response schema, so the caller falls
     * back to following the handler's return.
     */
    private declaredResponseInferredType;
    /**
     * The request contract of a route registration, in anchor order: the
     * handler's parameter annotation (a), the registration's declared schema (b),
     * then the first typed request read in the handler body (the pre-existing
     * behaviour). The first anchor yielding a non-useless type wins; null means
     * the route declares its request nowhere we can read.
     */
    private requestContractFromRegistration;
    /**
     * carrick-cloud#1366, shape 1: the request contract of a parameter a KEYED
     * parameter decorator binds to one field of the body (`@Body('key') x: T`).
     *
     * The decorator is recognised by shape, not by name: a call decorator whose
     * first argument is a string literal. Every parameter of the same handler
     * that the SAME decorator binds by key contributes one member, so two keyed
     * reads of one body publish one object. A sibling the same decorator binds
     * WITHOUT a key (no argument, or a non-string one such as a pipe) receives
     * the whole body, so the contract is that sibling's type intersected with
     * the keyed members.
     *
     * Returns null when the located node is not a keyed parameter, so every
     * other request path is untouched.
     */
    private keyedBodyParamContract;
    /**
     * The handler parameter a request locator names: the parameter itself, its
     * name, a decorator on it, or an identifier whose declaration is one. A
     * member read of a parameter (`x.length`) is not the parameter and is left
     * to the expression path.
     */
    private parameterNamedBy;
    /**
     * Every call decorator on the parameter, with the string key its first
     * argument names (`@Body('key')` -> `{ name: 'Body', key: 'key' }`). A call
     * decorator with no argument, or a first argument that is not a string (a
     * pipe instance), binds without a key.
     */
    private callDecoratorBindings;
    /**
     * carrick-cloud#1366, shapes 2 and 3: a request locator that names the
     * request CALL, or a request CONFIG argument, instead of the body.
     *
     * The body is found from the callee's DECLARED signature:
     *
     *  - a body slot is a parameter named as a body (`data`, `body`, `json`)
     *    and typed by one of the signature's own type parameters (`data?: D`);
     *  - a config slot is a parameter typed by a generic config type whose
     *    body-named member is typed by the config's own type parameter
     *    (`config?: Config<D>` with `data?: D`). A generic config with no such
     *    member (its type parameter types something else, a response mode say)
     *    is opaque: nothing can be said about its body.
     *
     * A located CALL is taken as the request call only when its first argument
     * is string-typed (the URL) and one argument supplies a body through a slot;
     * anything else is left alone, so an awaited payload builder keeps its own
     * type. A located argument in a config slot yields the payload member: from
     * an object literal directly, from a variable through its type. A literal
     * that does not set the member means the call sends no body; a variable
     * whose type does not resolve the member, or an opaque config, abstains.
     */
    private requestBodyInCall;
    /**
     * carrick#2628: the located node is an argument of a call into a function
     * whose body the program holds (an in-repo client method,
     * `client.addTags(project, unique(tags))`), at a parameter that is not a
     * body slot. What the call sends is decided inside that function, so the
     * argument is the body only when the function sends it unchanged. One
     * level, read off each use of the parameter in the function's body:
     *
     *  - the use IS the value of a body-named member (`body: tags`,
     *    `body: JSON.stringify(tags)`) or of a body slot of a call: the
     *    argument is the body, and its own type stands (`none`);
     *  - the use sits inside such a value (`body: { tags }`,
     *    `body: (event ?? {}) as never`): the body is that value, read at the
     *    function without the casts around it;
     *  - the use sits inside an object or array literal that is no body value
     *    (`send(project, [labels])`): the body cannot be told, abstain.
     *
     * Uses that disagree abstain. A generic function's body may be typed by
     * its type parameters, so it abstains too. A library's declaration
     * (`fetch`, an installed client) has no body to read and is left alone,
     * and so is an untyped argument (a handler's `request.body` handed to a
     * helper): it states no contract to mistake, and the untyped-read path
     * below reads it.
     */
    private bodyBuiltByCallee;
    /**
     * `bodyBuiltByCallee`, telling apart a callee that sends the argument
     * unchanged (`passes`) from one that states nothing about it (`none`).
     * `depth` counts the forwarded locals followed (carrick#2671).
     */
    private calleeBodyRead;
    /**
     * The request body value `use` is part of, inside `scope`: the value of a
     * body-named object member, or the argument at a body slot of a call,
     * looking through parentheses, casts, `await` and a `JSON.stringify`
     * around it. `use` itself when it is that value whole.
     */
    private bodyValueAround;
    /**
     * carrick#2671: the `const` local whose initializer holds `use`, inside
     * `scope`, when `scope` sends that local unchanged as the body: whole in a
     * body slot, or as an argument into a function that passes it on as its
     * body. The local's name, so its declared type is read. Undefined when the
     * local is never sent, when a further body is built from it, or when a
     * function lies between `use` and the declaration.
     */
    private forwardedLocalHolding;
    /** `value` is `use` behind what `sentValue` sees through. */
    private isSameValue;
    /**
     * The value a body slot sends when it sends anything, behind parentheses,
     * casts, `await`, a `JSON.stringify`, and (carrick#2668) a choice whose
     * other side sends nothing: `x ? JSON.stringify(x) : undefined`,
     * `x || null`, `x ?? undefined`. A `?? {}` sends `{}`, so it is a body the
     * function builds and is not seen through.
     */
    private sentValue;
    /**
     * The branch of `choice` that sends something, when its other branch sends
     * nothing (`undefined`, `null`, `void 0`, or `{}` spread into a request).
     */
    private sendingBranch;
    /** `undefined`, `null`, `void 0`, or (where `emptyObject`) `{}`. */
    private sendsNothing;
    /**
     * carrick#2668, #2664: `use` sits in the test of a choice whose other side
     * sends nothing (`body ? JSON.stringify(body) : null`,
     * `...(body === undefined ? {} : { body: JSON.stringify(body) })`). The
     * test only decides whether the argument is sent.
     */
    private inSendNothingTest;
    /** A string, a string literal, a template literal, or a union of strings. */
    private isStringLikeType;
    /**
     * carrick#1841, request half: a request call that states its URL on its one
     * object argument (`client.post({ url: '/items', ...options })`, the way a
     * generated client issues every operation). The method's own parameter
     * types the body `unknown`, so the object is the only statement of it: its
     * `body` member, the member `RequestInit` defines, written on the object or
     * carried in by a spread whose type says what it is.
     *
     * - A `body` written after the object's last spread is the body.
     * - Otherwise the object's own type has the answer. No `body` member, or
     *   one typed `undefined` or `never`, means the call sends no body.
     * - A member typed `any`, `unknown` or a type parameter, or one only the
     *   compiler's default library declares (a spread `RequestInit` carries the
     *   serialised `BodyInit`, not a payload), states no payload: abstain. So
     *   does an object that carries another body-named member and no `body`.
     *
     * An object with no string-typed `url` member is not a request object.
     */
    private requestObjectBody;
    /**
     * The body a config-slot argument supplies through `member`: an object
     * literal's own member, or a variable's member read off its type.
     */
    private configArgumentBody;
    /** A property whose name is computed, so the literal's written keys do not name it. */
    private isComputedMember;
    /**
     * A config literal that spreads another object, or writes a computed key,
     * names no `body` itself, so the literal's own type is read: the call sends
     * no body only when every branch of that type declares no `body` member, or
     * one typed `undefined` or `never`. The literal and each spread are first
     * read for a type that rules nothing out: one not known (a spread of an
     * untyped object), a generic one (a type parameter, or an intersection with
     * one), or one with a string index signature (which the compiler drops
     * from a spread's result). Any of those abstains.
     */
    private spreadLiteralBody;
    /** Why a spread-side type rules out no member, or undefined when it can be read. */
    private spreadTypeUnread;
    /**
     * Per parameter position of the call's resolved declaration: a body slot, a
     * config slot (with the member that carries the body), an opaque generic
     * config, or neither (undefined). Empty when the callee has no declaration.
     */
    private requestBodySlots;
    /**
     * A generic config type's slot: `config` with the body-named member typed by
     * the config's own type parameter (`interface Config<D> { data?: D }`), or
     * `opaque_config` when the config is generic but no body-named member is
     * (`interface Options<R> { responseType?: R }`). Undefined for a type that
     * is not a generic config, and for two candidate members (ambiguous).
     */
    private configSlot;
    /** The value an object literal gives `name`: an assignment's initializer or a shorthand's identifier. */
    private objectLiteralMemberValue;
    /**
     * The last member an object literal writes under `name`, in any form
     * (assignment, shorthand, method, accessor), with a quoted or numeric key
     * read by its text. Spreads and computed keys name nothing here.
     */
    private literalMemberNamed;
    /**
     * An explicit request `InferredType` for a contract the route declares,
     * carrying the provenance the reading recorded.
     */
    private declaredRequestInferredType;
    /**
     * The request contract a route DECLARES, in anchor order: the handler's
     * parameter annotation (a), the registration's schema object (b), then a
     * validator middleware bound to the request body (b2).
     *
     * Both callers — the registration locator and the expression locator whose
     * line is a registration — consult exactly this set, so the two cannot drift.
     * The typed request READ inside the handler body is deliberately not part of
     * it: that anchor is itself an expression, and the expression path keeps its
     * own locator authoritative when a route declares nothing.
     */
    private declaredRequestContract;
    /** The stated-parameter reader over `labels`, judging files and statuses as this inferrer does. */
    private statedParamsFor;
    /**
     * carrick#2674: where today's reading of a route's response is empty
     * (nothing, `void`, or an `unknown` the inferrer did not decide), the reply
     * its handler's parameter states, cut to its success variants. A typed
     * reading, or a decided abstain, stands as it is.
     *
     * carrick#3054: a route whose receive op carries a `returns` label replies
     * with what its handler returns. A labelled send the handler's parameters
     * state wins over the return. The returned reply replaces a typed reading
     * only where it prints differently (the label names a part of the return,
     * or a nothing part is dropped); where it prints the same, today's reading
     * stands with everything it carries.
     */
    private withStatedReply;
    private publishStatedReply;
    /** Nothing read: `void`, or an `unknown` with no decided reason at its root. */
    private isEmptyResponseReading;
    /**
     * Anchor (a): the request contract declared on the handler's own signature.
     *
     * A typed request parameter (`request: CreateWidgetRequest` where that alias
     * resolves to a request type parameterised with the body shape) exposes the
     * contract as the `body` member of the parameter's type. Parameters are
     * scanned in order and the first one carrying a non-useless `body` member
     * wins; a reply/response parameter has no `body` member, and an unparameterised
     * request type resolves `body` to `unknown`/`any`, which the useless-type
     * guard rejects. So a handler that declares nothing yields null and the next
     * anchor runs.
     *
     * The member has to be one the route's own annotation can have filled
     * (carrick#807). The platform `Request` declares `body` too, as the byte
     * stream every request carries, and so does each library type that extends
     * it. That is concrete, so it passed the useless-type guard and a handler
     * taking the platform request published a stream as its request contract,
     * ahead of the body read inside it. A `body` a library declares with one
     * fixed type says nothing about this route and is skipped.
     */
    private requestBodyFromHandlerParams;
    /**
     * True when a member is declared by a library, or by the platform, with a
     * type that names none of its declaring type's parameters: the same type on
     * every value, whichever route the value belongs to.
     *
     * `body: ReqBody` on a request type generic in its body is a slot, and the
     * handler's annotation fills it. `readonly body: ReadableStream<Uint8Array>
     * | null` on the platform request is not. A member the repo declares itself
     * is never fixed in this sense: writing `body: NewWidget` on the handler's
     * own request type is the annotation, and one declaration of the repo's
     * among several (an intersection with the platform type) decides it. A
     * declaration that states no type at all is left to the guards that read
     * the type.
     */
    private memberIsFixedByItsLibrary;
    /**
     * Anchor (b2): the contract declared by a VALIDATOR MIDDLEWARE on the
     * registration (carrick#964).
     *
     * A route can declare its body by binding a schema to a named part of the
     * request in a middleware the registration carries alongside the handler:
     *
     *   router.post('/search', validate('json', PayloadSchema), async (c) => …)
     *
     * The shape is a CALL among the registration's arguments whose own arguments
     * are a request part and a schema value. Neither the middleware's name nor
     * the schema library is checked: the part is read off the string literal, and
     * the schema is whatever declares its types (`schemaContract`) — the same
     * test anchor (b) already applies to a `schema: { body: … }` object. The
     * contract is the schema's INPUT, what a caller sends (carrick#1101).
     *
     * `REQUEST_BODY_PARTS` is HTTP vocabulary for how a body is carried, not a
     * framework list. A middleware bound to any other part (`query`, `param`,
     * `header`, `cookie`) declares no BODY, and a middleware that names no part
     * at all is not read: publishing a query schema as the request contract would
     * be the same confident-and-wrong answer this anchor exists to remove.
     */
    private validatedBodyContract;
    /**
     * The values a registration's validator middleware binds to a body part, in
     * argument order: every non-literal argument of a middleware call that names
     * a `REQUEST_BODY_PARTS` part. Whether each one IS a schema is the reader's
     * question (`schemaContract`), not this walk's.
     */
    private middlewareBodySchemaValues;
    /**
     * The type the function a value is made in states for it, where the
     * compiler types the value itself `any` or `unknown` (carrick#2594), or
     * null.
     *
     * `value` is the located expression: a body read, or a call whose result is
     * untyped. The value is followed through the names that hold it: a
     * declaration it initialises, and a name it is assigned to later
     * (`let body: unknown; body = await read()`). At each place the value or
     * one of those names is used whole, the source can state a type:
     *
     *  0. the annotation of the name that holds it (`let body: Upload`), or the
     *     declared return type of the function that returns it unchanged (only
     *     with `reads.returned`);
     *  1. a schema it is handed to (`Schema.safeParse(body ?? {})`,
     *     `parse(Schema, body)`): the schema's input is what a sender may send
     *     (carrick#1166). A candidate counts only when it declares schema types,
     *     so no method or library name is matched;
     *  2. a function of the project it is handed to that returns that argument
     *     unchanged (through casts) or an empty default: its declared return
     *     type. A function that builds a new value from its argument is a
     *     converter, and its return type describes what it built, never the
     *     value it was handed, so it states nothing here;
     *  3. a cast of the whole value (`(body as { phone: string }).phone`);
     *  4. the parameter of a call it is handed to (only with
     *     `reads.parameters`), where that parameter's type is a body shape once
     *     any callable member is dropped (a function never crosses the wire).
     *
     * The first rank with an answer decides. A placeholder (`unknown`,
     * `Record<string, unknown>`, any type node with `any` or `unknown` written
     * in it) states nothing, and two different statements in one rank decide
     * nothing: the answer is null.
     */
    private statedInFunction;
    /**
     * What a call that takes the value at argument `index` states for it:
     * a schema among its receiver and other arguments (rank 1), a callee of the
     * project that returns that argument unchanged (rank 2), or, with
     * `reads.parameters`, the parameter's own type (rank 4).
     */
    private stateAtReceivingCall;
    /**
     * The input contract of a schema `call` validates its argument at `index`
     * with: its receiver (`Schema.safeParse(body)`) or one of its other
     * arguments (`parse(Schema, body)`), or null. A candidate counts only when
     * it declares schema types, so no method or library name is matched
     * (carrick#1166).
     */
    private schemaHandedArgument;
    /**
     * `call`'s result describes the value it takes at argument `index`: it
     * validates it with a schema, or its callee returns it unchanged
     * (`passThroughFunction`). Any other call builds something new from the
     * value, and its result describes what it built, never the value it was
     * handed (carrick#2595).
     */
    private callStatesItsArgument;
    /**
     * The project function `call` invokes, when every value it returns is its
     * argument at `index` as it was handed in (through parentheses, casts,
     * `satisfies` and `!`) or an empty default (`{}`, `[]`, `null`,
     * `undefined`), and at least one return is that argument. Such a function
     * narrows its argument, so its declared return type describes the value it
     * was handed. Any other function builds something new from it, and is not
     * this.
     */
    private passThroughFunction;
    /** `{}`, `[]`, `null` or `undefined`: a default that states no member. */
    private isEmptyDefault;
    /**
     * The part a validated read names when that part is not a body, or
     * `undefined` (carrick#1166).
     *
     * The read is a call whose only argument is a string literal, and the
     * registration carries a validator middleware binding that same literal.
     * The part names match through the source's own literal, so no framework
     * vocabulary is assumed beyond `REQUEST_BODY_PARTS`, which already decides
     * what a body is for the middleware anchor.
     */
    private nonBodyValidatedPart;
    /**
     * The request contract behind a VALIDATED READ: a call inside a registered
     * handler whose type is exactly the parsed output of a body schema the
     * enclosing registration declares (carrick#1101).
     *
     * The read's own type is what validation hands the handler, the schema's
     * output, so a locator that lands on it would publish the output as the
     * request. The registration is found by walking the read's ancestors to each
     * call or registry object whose handler contains the read and which declares
     * a body schema (anchor (b) or (b2)). The match is on the printed type: only
     * a read whose structural text equals that schema's output is substituted, so
     * any other expression in the handler (a query read, a payload of a different
     * shape) keeps its own type.
     */
    private validatedReadRequestContract;
    /**
     * Anchor (b): the contract declared in the route's validation schema.
     *
     * `part` is `'body'` for the request contract and `'response'` for the
     * response contract; a `response` entry keyed by status code resolves to its
     * success entry. The body reads the schema's input and the response its
     * output (carrick#1101). Returns null when the registration carries no
     * schema, when the entry references something whose declared types cannot be
     * resolved, or when the schema is a plain JSON-schema literal (whose own
     * object type is the JSON-Schema document, not the payload — emitting that
     * would be worse than abstaining).
     */
    private routeSchemaContract;
    /**
     * The value a route `schema` object declares for `part`, as a zero- or
     * one-element list: the `body` entry itself, or the success entry of a
     * status-keyed `response` map.
     */
    private routeSchemaEntryValues;
    /**
     * The `schema` object literal carried by a route registration: scan the
     * registration call's arguments (or the registry object literal itself) for a
     * `schema` property whose value is an object literal.
     */
    private routeSchemaObject;
    /**
     * The success entry of a status-keyed response map (`{ 200: …, 4xx: … }`):
     * exact `200` when present, else the lowest 2xx key. Returns undefined when
     * the value is not a status-keyed map, so the caller reads it as the schema
     * itself.
     */
    private successStatusEntry;
    /**
     * The payload type a schema entry declares, read in `direction`.
     *
     * The entry is either a REFERENCE call — `ref('CreateWidget')`, a
     * name-to-schema indirection whose registry is the argument the ref function
     * was built from — or the schema value itself. A value that declares no type
     * in either direction is not a schema and yields null.
     *
     * `output` is the parsed value (`schemaOutputType`). `input` is what a caller
     * sends (`schemaInputType`), and a schema that declares no input member reads
     * its output instead, which is the whole of what is knowable about it.
     *
     * The input is decided member by member (carrick#1105). A member whose input
     * is `any`/`unknown` where the output is concrete is what a coercion declares
     * (it accepts any value and parses it into, say, a number). A top type in a
     * published contract disqualifies the whole row downstream, so THAT member
     * prints its output type, keeps the input key's optionality, and is recorded
     * as `coerced_input` provenance. Every other member keeps its input, so a
     * defaulted key beside a coerced one stays optional to send.
     *
     * Limits, each logged where it bites:
     *  - the position walk is bounded (`walkTypePositions`), so a top type below
     *    the bound is not compared and prints as the input declares it;
     *  - a coerced position that cannot be substituted (its output is absent or
     *    differs across union branches, or the printer cannot reach it inside a
     *    tuple, behind a cycle or past the expansion depth) makes the row fall
     *    back to the whole parsed output with every coerced member labelled. That
     *    was the answer before the per-member reading, and it never publishes an
     *    `unknown`.
     */
    private schemaContract;
    /**
     * Print a schema's input with each coerced position replaced by the output's
     * type at that position (carrick#1105). Null when any position cannot be
     * substituted: its output is missing or differs across union branches, or
     * the printer never reached it. A partial substitution would publish an
     * `unknown`, so it is not an answer.
     */
    private inputWithCoercedMembers;
    /**
     * Member positions inside `root` that are `any` or `unknown`, keyed by the
     * member-path notation the provenance entries use (`''` for the root,
     * `sub.field`, `items<0>` for an array element). Union and intersection
     * members share their parent's position, so `unknown | undefined` on an
     * optional key reads as `unknown` there.
     */
    private topTypePositions;
    /**
     * Visit every member position of `root`, in the notation `topTypePositions`
     * documents. `visit` returns false to stop descending below a position;
     * `unionPart` is true when the type is a union or intersection part visited
     * at its parent's position. Callables are not descended into.
     *
     * Bounded and cycle-safe. It only has to tell a schema's input apart from its
     * output, so a subtree past the bound is not compared, and that is logged:
     * a coercion below the bound prints as its input declares it.
     */
    private walkTypePositions;
    /**
     * Follow a schema REFERENCE call (`ref('CreateWidget')`) to the schema value
     * it names.
     *
     * The ref function is produced by a registry-building call — `const { $ref } =
     * build({ CreateWidget, Widget })` — so the registry is that call's first
     * argument, and the referenced key is a property of it. Resolve the callee to
     * its declaration, walk to the variable declaration it is bound in, and read
     * the key off the builder argument's type. Returns undefined for anything
     * that is not this shape.
     */
    private registrySchemaType;
    /**
     * The parsed output type of a schema value: the return type of its `parse`
     * method, else a declared `_output` member, else the Standard Schema member
     * `~standard.types.output` (a library that exposes nothing else). Returns
     * undefined when none carries a usable type — including when the schema library's own types are
     * unavailable (an uninstalled dependency resolves the schema to `any`), which
     * must abstain rather than publish `any` as a contract.
     */
    private schemaOutputType;
    /**
     * The input type of a schema value, what a caller may send (carrick#1101):
     * the Standard Schema member `~standard.types.input`, else a declared
     * `_input` member. Undefined when the schema declares neither.
     *
     * Unlike the output, an input of `unknown` is kept: it is a declared fact
     * (a coercion accepts any value), and `schemaContract` decides what to
     * publish for it. An `any` that is really an unresolved library still reads
     * as no input, because an unresolved schema type has no members to read.
     */
    private schemaInputType;
    /**
     * `~standard.types.<side>` on a schema value: the Standard Schema
     * specification's library-neutral statement of a schema's input and output
     * types. `types` is declared optional, so its `undefined` is stripped before
     * the side is read.
     */
    private standardSchemaType;
    /**
     * Find a node by matching expression text near a target line.
     *
     * Strategy:
     * 1. Get all descendant nodes within [lineNumber - searchRadius, lineNumber + searchRadius]
     * 2. Normalize whitespace for comparison
     * 3. Try exact match first (after normalization), prefer closest to target line
     * 4. Fall back to substring match (LLM text in node text, or vice versa)
     * 5. Return smallest matching node closest to target line
     */
    private findNodeByText;
    /**
     * Find a CallExpression by matching expression text near a target line.
     */
    private findCallExpressionByText;
    /**
     * Shared text-matching logic for node resolution.
     * Normalizes whitespace once per candidate, then tries exact match,
     * then substring match (preferring containing matches).
     */
    private matchByText;
    /**
     * The text a locator opens with before an elision, or `undefined` when the
     * locator has none. An elision is `...` followed only by closing brackets
     * (`{ a: x, ... }`); `...rest` is a spread and source text. An opening too
     * short to tell nodes apart is judged by the caller.
     */
    private elidedLocatorOpening;
    /**
     * What a locator's text names, read off its own parse (carrick#2056):
     *
     *  - `texts`: the normalized text of the expression it names and of each
     *    wrapper it carries around it (`return`, a `const x =` binding, a label,
     *    `await`, parentheses, a cast, `satisfies`, `!`). A node strictly inside
     *    that expression is not among them;
     *  - `rootKind` and `rootStart`: the innermost of those, its kind, and where
     *    its text starts in the locator.
     *
     * `null` when the text does not parse as one expression or one statement:
     * there is no structure to read, and the caller keeps its text rule.
     */
    private locatorNames;
    /**
     * Pick the best match from a set of candidate nodes:
     * smallest range, then closest to target line.
     */
    private pickBestMatch;
    /**
     * Normalize whitespace for text comparison:
     * collapse runs of whitespace into single spaces, trim.
     *
     * Also strips trailing commas before `}` `)` `]`: multi-line source
     * literals carry them but the LLM's single-line locator print does not,
     * and that one comma used to defeat exact AND containment matching for
     * the payload and every enclosing node (#335). Applied symmetrically to
     * node text and target, so both sides compare equal.
     *
     * Also strips the space left AFTER `(` `[` `{` by the collapse: a
     * multi-line call whose arguments start on the next line normalizes to
     * `f( x)` while the LLM's compact print is `f(x)`, and that one space
     * defeated exact matching for the call and every enclosing node — the
     * opening-delimiter mirror of the #335 trailing comma (#336). Symmetric
     * for the same reason.
     */
    private normalizeWhitespace;
    /**
     * Format a human-readable location string for error messages.
     */
    private formatRequestLocation;
    private findContainingFunctionBySpan;
    private findNodeAtSpan;
    private findCallExpressionAtSpan;
    private unwrapPromise;
    /**
     * Unwrap a single `Promise<...>` / `PromiseLike<...>` type string, only when
     * the inner text is bracket-balanced (so `Promise<A> | B` is left alone for
     * the caller's union handling rather than mangled).
     */
    private unwrapPromiseText;
    /**
     * Split a type string on `|` at bracket depth 0. `=>` is not treated as a
     * closing bracket.
     */
    private splitTopLevelUnion;
    private isBracketBalanced;
    private getNodeLocation;
    private createInferredType;
    private generateAlias;
    private inferKindSuffix;
    private log;
    private logError;
}
