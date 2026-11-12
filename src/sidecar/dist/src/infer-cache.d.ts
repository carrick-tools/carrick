/**
 * An `infer` answer kept on disk and served again while nothing it was read
 * from has changed (carrick#2843).
 *
 * A scoped re-scan re-analyses a service when one of its imports changed,
 * even when nothing the service's own types are read from did. A generated
 * API client asks hundreds of `infer` requests of a program that a change
 * elsewhere does not touch, and answering them costs tens of seconds each
 * time. This keeps the last answer per service and serves it when the
 * program it was read in is the same program.
 *
 * What keys an answer, all of it compared before one is served:
 * - the sidecar itself: its compiled code, the TypeScript and ts-morph
 *   versions it runs and the Node version (`sidecarIdentity`);
 * - the `init` request the process was scoped with, less its id;
 * - the `infer` request, less its id: every item, the extraction config, the
 *   model symbols and the server claims;
 * - for each program the request is answered in: its root files in order,
 *   its compiler options, and every source file the compiler holds, by path
 *   and by the sha256 of its text (`programRecord`).
 *
 * The program is the compiler's whole view: a declaration it could read is a
 * source file in it, and a file that would now resolve in place of another
 * changes the list. To compare it, the lookup builds each program afresh from
 * its tsconfig, adds the roots the stored answer's program had, in order, and
 * compares the result. A program that differs is dropped again, so the
 * inference that follows builds it exactly as it would have with no cache.
 * A program that matches is the one the inference would have left behind, so
 * every later request in the process reads the program it would have read.
 *
 * Not kept: an answer whose unwidened reading ran out of its time budget (it
 * depends on the clock), an answer that read server-framework claims (they
 * are read in a program of their own, which the record does not cover), and
 * an answer of a process whose program a request before it had built (its
 * roots then depend on that request).
 *
 * On only where `CARRICK_INFER_CACHE_DIR` names a directory: local scans set
 * it, and a CI scan, which starts on a clean machine, does not.
 */
import type { Project } from 'ts-morph';
import type { InferredType } from './types.js';
/** The variable naming the directory answers are kept in. */
export declare const INFER_CACHE_ENV = "CARRICK_INFER_CACHE_DIR";
/** One program an answer was read in, as `programRecord` saw it. */
export interface ProgramRecord {
    key: string;
    /** The program's root files, in order. */
    roots: string[];
    /** Its compiler options, as JSON. */
    options: string;
    /** Every source file the compiler holds: path, module format, sha256 of its text. */
    sources: Array<[string, string, string]>;
}
/** The parts of an `infer` answer that are kept and served again. */
export interface KeptAnswer {
    status: 'success' | 'error';
    inferred_types?: InferredType[];
    errors?: string[];
}
/** The directory answers are kept in, or `undefined` when the cache is off. */
export declare function inferCacheDir(): string | undefined;
/**
 * The sidecar that reads the answers: every compiled file of its own, the
 * compiler and ts-morph versions it loads, and the Node version.
 */
export declare function sidecarIdentity(): string;
/** A request's digest, less the id every request carries. */
export declare function requestDigest(request: object): string;
/**
 * What `now` differs from `kept` in, first difference only, or `undefined`
 * when they are the same program: the same roots in the same order, the same
 * options, and the same source files with the same text.
 */
export declare function programDifference(kept: ProgramRecord, now: ProgramRecord): string | undefined;
/** What a kept answer is read against: the process and the request. */
export interface AnswerKey {
    /** The directory entries are kept in. */
    dir: string;
    /** The service root the process was scoped to: one entry per root. */
    repoRoot: string;
    /** `requestDigest` of the `init` request the process was scoped with. */
    init: string;
    /** `requestDigest` of the `infer` request. */
    request: string;
}
/**
 * The kept entry for `key` when the sidecar, the scope and the request are
 * the ones it was kept for. The programs are not compared here: that needs
 * them built (`servedAnswer` in `index.ts`).
 */
export declare function readEntry(key: AnswerKey): {
    programs: ProgramRecord[];
    answer: KeptAnswer;
} | undefined;
/** Whether `answer` can be served again: it does not depend on the clock. */
export declare function keepable(answer: KeptAnswer): boolean;
/**
 * Everything the compiler holds for `project` as it stands, under `key`: its
 * root files in order, its options, and each source file's path, module
 * format and the sha256 of its text.
 */
export declare function programRecord(key: string, project: Project): ProgramRecord;
/**
 * The first source file of `kept` whose text on disk is not the text it was
 * read with, or `undefined` when every one on disk is the same. Read before any
 * program is built: a changed file, the usual reason an answer is stale, is
 * found for the cost of reading the files, and no program is built to find
 * it. The compiler reads a file without its byte-order mark, and so does
 * this.
 */
export declare function changedSource(kept: ProgramRecord): string | undefined;
/**
 * Keep `answer` for `key`, read in `programs`. Written to a temporary file
 * and renamed, so a reader never sees half an entry. Entries not written to
 * for `ENTRY_MAX_AGE_MS` are deleted on the way.
 */
export declare function writeEntry(key: AnswerKey, programs: ProgramRecord[], answer: KeptAnswer): void;
