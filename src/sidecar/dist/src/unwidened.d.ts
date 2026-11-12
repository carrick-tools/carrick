/**
 * The unwidened reading of a producer's response (carrick#1516), and of a
 * consumer's request body (carrick#2011).
 *
 * A handler that returns `{ scope: row.parentId ? 'specific' : 'all' }` sends
 * one of two strings, but its inferred return type says `scope: string`:
 * TypeScript widens a literal that lands in a mutable position (an object
 * property, an array element, a function's return). The index publishes that
 * widened type, so a consumer declaring `scope: 'all' | 'specific'` is judged
 * against a `string` the handler never sends.
 *
 * The published type stays what the compiler inferred. Beside it, this reads
 * the SAME inference again with every literal on the handler's path marked
 * `as const`, which is the compiler's own way of saying "keep this literal's
 * type". A const-asserted literal is a regular literal type, and only fresh
 * literal types widen, so the re-read inference is the handler's return with
 * nothing widened: `'all' | 'specific'` for the conditional, `true`/`false`
 * for a discriminant, `2 | 1` for a numeric choice. No rule of ours decides
 * what a literal becomes; the compiler infers the return again.
 *
 * The judge uses the reading only to classify a mismatch the published type
 * already raised: when this narrower type fits the consumer, the pair is
 * reported as the producer's type being wider than what it sends, not as a
 * break.
 *
 * Soundness rests on three things:
 *
 * - **A literal that feeds a mutable binding keeps its widening.** `let s =
 *   'a'` can later hold `'b'`, so its type really is `string`; the literals
 *   that initialise a `let`/`var` are never marked. Nor is a parameter's
 *   default: it decides what callers may pass, and a caller the path does not
 *   reach could pass another value.
 * - **No new diagnostic on the handler's path.** A marked literal that is
 *   later changed (an object built with `'a'` and then assigned `'b'`) makes
 *   the compiler report the assignment. Every file holding a function on the
 *   path is type-checked before and after, and any diagnostic the marking adds
 *   drops the reading for every request whose path includes that function.
 * - **The re-read came from the same place.** The inference is re-run from
 *   the request, so its locator runs again; the reading is kept only when it
 *   read the same node it read the first time.
 *
 * The path is the function the inference read (or the function around the
 * payload expression it read), plus every source function it calls whose
 * return type is inferred rather than declared, transitively: a controller
 * that returns `this.service.list()` sends what `list` returns. A callee with
 * a declared return type is not followed; that declaration is the contract.
 */
import { Node } from 'ts-morph';
import type { SourceFile } from 'ts-morph';
/** One insertion into a file's ORIGINAL text. */
export interface Insertion {
    pos: number;
    text: string;
    /**
     * An opener sits before the literal it wraps, a closer after it. At one
     * position a closer (ending the previous literal) comes before an opener.
     */
    closes: boolean;
}
/** A function on a request's path, by its original span. */
export interface PathFunction {
    file: SourceFile;
    start: number;
    end: number;
}
/** One diagnostic a file's type-check reported. */
export interface FileDiagnostic {
    /** Start position in the file it was reported on. */
    start: number;
    code: number;
    message: string;
}
/**
 * The functions on the path of a read node: the function it is (or sits in),
 * then every un-annotated source function called from those, breadth first.
 * A read node outside any function is its own path.
 */
export declare function pathOf(read: Node): Node[];
/**
 * The insertions that mark every literal inside `fn` whose widening the
 * reading drops. Positions are original-file positions.
 *
 * `sent` is given for a request body a consumer sends (carrick#2011), and is
 * the expression the inference read. A literal handed to any other call is
 * then left alone: keeping `useState('open')` narrow narrows the state's
 * setter too, and code elsewhere that calls the setter with another value
 * sits in a file the reading's guard never checks. A response's reading
 * passes no `sent`: what the handler hands its callees is part of what it
 * returns.
 */
export declare function literalInsertions(fn: Node, sent?: Node): Insertion[];
/** Deduplicated, in text order (a closer before an opener at one position). */
export declare function normalise(insertions: Insertion[]): Insertion[];
export declare function applyInsertions(text: string, insertions: Insertion[]): string;
/**
 * Where an original position lands in the rewritten text. A START keeps an
 * opener at its own position outside (so a span starting on a marked literal
 * covers its parentheses); an END takes every insertion at its position in.
 */
export declare function mapForward(pos: number, insertions: Insertion[], side: 'start' | 'end'): number;
/** The original position a rewritten one came from; inside an insertion, its position. */
export declare function mapBack(pos: number, insertions: Insertion[]): number;
/** Syntactic and semantic diagnostics reported in this file. */
export declare function fileDiagnostics(sourceFile: SourceFile): FileDiagnostic[];
/**
 * Original positions of the diagnostics `after` holds that `before` does not,
 * matched by original position and code. A diagnostic whose message changed
 * because a type narrowed is the same diagnostic.
 */
export declare function addedDiagnostics(before: FileDiagnostic[], after: FileDiagnostic[], insertions: Insertion[]): number[];
