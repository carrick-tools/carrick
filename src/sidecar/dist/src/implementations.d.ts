/**
 * The implementations of a bodyless declaration, asked where discovery stops
 * at one (carrick#2432 slice B): a call through an interface's method, a
 * function-typed property or an abstract member, whose receiver the scanner
 * cannot follow to a value.
 *
 * Each question is the span of a `call` (or `member`) position row, in the
 * rows' numbering (UTF-16 code units from the start of the file, as the
 * scanner's `sidecar_position` gives). The call's symbol and declaration are
 * read exactly as the row read them ({@link PositionWalker.symbolOf}), and
 * go-to-implementation is asked once per declaration, at its name, over the
 * language service's program of the project that owns the file. That is the
 * analysis program: nothing here releases it.
 *
 * Each answer:
 * - holds none of the declaration's own signatures: asked at an overloaded
 *   interface or abstract method, go-to-implementation names only the bodies
 *   that implement it (TypeScript 6.0, pinned in implementations.test.ts).
 *   The walk's filter for them (`implementationRows`, removed in #2561) is
 *   not carried over: no bodyless declaration reaches it;
 * - names each implementation as a row names a declaration
 *   ({@link PositionWalker.located}), at the implementation's declaration's
 *   start, so it joins to a definition as a `call` row at it would;
 * - is sorted by file, then position;
 * - holds at most {@link IMPLEMENTATIONS_CAP}; past it, none, and `too_many`.
 *
 * The position rows carry no implementations (version 4, carrick#2561): this
 * action is the only place they are asked.
 */
import type { ProjectLoader } from './project-loader.js';
import type { ImplementationsAnswer, ImplementationsQuery } from './types.js';
/** The most implementations one answer lists. */
export declare const IMPLEMENTATIONS_CAP = 16;
export interface ImplementationsOptions {
    loader: ProjectLoader;
    /** Every name in an answer is relative to it, as in the position rows. */
    scanRoot: string;
    queries: ImplementationsQuery[];
    /** Called after each question is answered. */
    onAnswer?: () => void;
}
/** One answer per question, in request order. */
export declare function findImplementations(options: ImplementationsOptions): ImplementationsAnswer[];
