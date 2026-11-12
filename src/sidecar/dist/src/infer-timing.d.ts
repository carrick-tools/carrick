/**
 * Which requests of an `infer` batch were slow, and at what (carrick#1985).
 *
 * A pass can send thousands of requests and spend most of its time in a few
 * of them. The inferrer times every request; this module turns those timings
 * into what the batch's answer carries: how many there were and what they
 * add up to, the slowest few by name, the request that printed the longest
 * type, and the requests that were the first asked of their file, apart.
 *
 * A request's time is also told apart by what it went on: the compiler call
 * that computes the type, and the print of that type to text. The two have
 * different remedies, and a printed length alone cannot say which one a slow
 * request was: a type of four characters can take seconds to compute, and a
 * type of a million can take half a second to print.
 *
 * It measures and names. It changes no answer and holds no type text.
 */
import type { InferSlotTiming, InferTiming } from './types.js';
/**
 * How many of its slowest requests a batch's answer names.
 *
 * Enough for a reader adding batches up to say what the slowest hundredth of
 * a whole pass took: a pass's slowest requests are among its batches'
 * slowest, and the last one a batch names is the most any it left out can
 * have taken.
 */
export declare const SLOWEST_SLOTS = 25;
/** How long something that started at `startedMs` has taken by `nowMs`. */
export declare function elapsedMs(startedMs: number, nowMs: number): number;
/**
 * The parts of a request's time that are told apart: `type` is the compiler
 * call that computes a type, `print` is the print of a type to text.
 */
export type TimedPhase = 'type' | 'print';
/**
 * Run `work`, and count how long it took as `phase`.
 *
 * The count is the process's, not a request's: whoever wants one request's
 * share reads `phaseClock` before the request and after it. One request runs
 * at a time, so the difference is that request's and nobody else's.
 */
export declare function timedPhase<T>(phase: TimedPhase, work: () => T): T;
/** What each phase has come to so far, in milliseconds. */
export declare function phaseClock(): Readonly<Record<TimedPhase, number>>;
/**
 * What a batch's answer says of the time its requests took. `slots` holds one
 * timing per request, in the order the batch was done with them.
 */
export declare function inferTiming(slots: readonly InferSlotTiming[]): InferTiming;
/**
 * One timing for a request several projects answered, each with its own:
 * the counts and the times added up, and the slowest of all of them.
 */
export declare function mergeInferTimings(parts: readonly InferTiming[]): InferTiming;
