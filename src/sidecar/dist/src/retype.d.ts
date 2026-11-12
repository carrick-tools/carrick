/**
 * The retype check (carrick#1491): judge an untyped consumer call by what its
 * own source does with the producer's response.
 *
 * A consumer that writes `await api.post('/orders')` with no type argument
 * gets `any` back, so the pair has nothing to compare and every field it reads
 * goes unchecked. The source still says how the response is used: the members
 * it reads, what it destructures, the typed places it flows into. So the call
 * is rewritten, in memory, to state the producer's response type
 * (`api.post<ProducerResponse>('/orders')`), the consumer file is type-checked
 * before and after, and every diagnostic the rewrite adds is a place where the
 * consumer uses something the producer does not return. No rule of ours
 * decides anything: the compiler reads the consumer's own code.
 *
 * A call that takes no type argument but returns a typed transport response
 * (`const res = await fetch(u)`) is judged at its body read instead: the
 * `res.json()` on that call's result is the payload, so that read is cast to
 * the producer's type (carrick#1493).
 *
 * Runs in the consumer's REAL program (the init'd project), because that is
 * the only place its file can type-check: the check workspace holds
 * declaration stubs, not consumer sources, and none of the consumer's imports.
 *
 * Every edit is undone before the next item and before returning, so the
 * project other requests read is the project the scan loaded.
 */
import { ts } from 'ts-morph';
import type { Project } from 'ts-morph';
import type { TypeInferrer } from './type-inferrer.js';
import type { RetypeItem, RetypeLocatorItem, RetypeOutcome, RetypePrescreen } from './types.js';
/**
 * The check phase's structural any/unknown walk (`findDisqualifyingTopTypes`
 * in the capture bundle): member paths below `root` that hold a top type.
 */
export type TopTypeWalk = (root: ts.Type, program: ts.Program, checker: ts.TypeChecker, location: ts.Node) => Array<{
    kind: string;
    path: string;
}>;
export declare class Retyper {
    private readonly project;
    private readonly inferrer;
    private readonly jsonWire;
    private readonly topTypes;
    /**
     * `jsonWire` is the check phase's own JSON wire transform
     * (`jsonWireDeclarations` in the capture bundle) and `topTypes` its
     * any/unknown walk, handed in by the entry point so both judges read the
     * wire and a top type the same way without this file crossing the bundle
     * seam.
     */
    constructor(project: Project, inferrer: TypeInferrer, jsonWire: (prefix: string) => string[], topTypes: TopTypeWalk);
    /**
     * Judge every item, spending at most `budgetMs`. Each item rebuilds the
     * program at least twice, so a consumer with many calls could otherwise
     * outrun the caller's read deadline and lose every answer; the items the
     * budget does not reach abstain and say so. `onJudged` is called after each
     * item the budget reached, so the caller can report progress while the
     * event loop is blocked (carrick#1945).
     */
    run(items: RetypeItem[], budgetMs: number, onJudged?: () => void): RetypeOutcome[];
    private runOne;
    /**
     * Per item, the reason `run` abstains for before any rewrite, which no
     * producer type changes, or `undefined` when only the rewrite can decide
     * the call (carrick#3132). The scan asks this in the consumer's own
     * analysis, so a consumer whose calls all abstain here builds no retype
     * program. A call that throws here is left to the retype, which reports
     * the failure itself.
     */
    prescreen(items: RetypeLocatorItem[]): Array<Omit<RetypePrescreen, 'item_id'>>;
    /**
     * The call and how to state a type at it, or why the retype abstains
     * before any rewrite. Reads only the consumer's own code: the producer's
     * type is stated by the returned plan.
     */
    private beforeRewrite;
    /**
     * Why the call does not state the response when the type argument the
     * retype would state there (its first, written or inferred) holds a type
     * parameter of a declaration the call sits in (carrick#3897), or
     * `undefined`. `return send<T>(url)` inside `read<T>(): Promise<T>` is a
     * pass-through: the call site cannot bind `T`, and the response is read
     * where `read` is called. Stating the producer's type there only makes the
     * compiler compare it with the open `T` ("'T' could be instantiated with an
     * arbitrary type"), which no reader of the response wrote. An enclosing
     * parameter in any other type argument (a generated client's mode flag)
     * leaves the response stated, and the call is judged.
     */
    private unboundTypeParameterReason;
    /**
     * The rewrite that states the producer's UNWIDENED return (carrick#1516),
     * when the item carries one that can be stated at this call.
     */
    private unwidenedPlan;
    /**
     * How to state a producer's type at this call, or why it cannot be done
     * whatever that type is. The builder it returns takes the type, and then
     * builds the wire and the declared form from one decision.
     */
    private plan;
    /**
     * Apply one rewrite, type-check, restore. Returns the diagnostics the
     * rewrite ADDED, in original-file positions.
     */
    private check;
    /**
     * Why the stated type, as the consumer's program reads it, cannot be
     * compared, or `undefined` when it can (carrick#1514). Read after the
     * diagnostics diff, because the two top types fail in opposite directions.
     *
     * The producer's text is sent only when it holds no `any`/`unknown`, but
     * the consumer's program is where it is read: under its compiler options,
     * through the wire transform (a lib type whose `toJSON()` returns `any`),
     * with its own declarations.
     *
     * - A member that reads as `unknown` makes reads fail that the producer's
     *   type would pass, so any diagnostic may be ours. It is never compared.
     * - A member that reads as `any` can hide a failure but never make one, so
     *   the diagnostics the diff found stand; only an empty diff is not
     *   compared, since it may be the `any` agreeing.
     *
     * Either way the reason names the member, as the check phase does for a
     * published type.
     *
     * The walk's budget sentinel is dropped. The scanner's text screen
     * (`contains_disqualifying_top_type`, which has no budget) found no
     * `any`/`unknown` in the producer's text before sending it, so a walk that
     * runs out of budget can only miss one the consumer's program made deeper
     * than the budget allows; the compiler's diagnostics stand.
     */
    private unresolvedMembers;
    /**
     * The stated type argument must be what the call RETURNS (or a type
     * argument or member of it): the rewrite is only a statement about the
     * response when the parameter it fills carries the response. A parameter
     * that types something else — a request body, a config — would judge the
     * consumer against the wrong thing, so the item abstains.
     */
    private typeArgumentReachesResult;
    /** What the call yields once awaited: the type the reach test compares. */
    private resultOf;
    /**
     * The abstention for a call whose type argument is not in its result. The
     * one place the sentence is built: the reach test gives it after the
     * rewrite, and the prescreen gives it before one (carrick#3158), so the two
     * can never word it differently.
     */
    private notReachedReason;
}
