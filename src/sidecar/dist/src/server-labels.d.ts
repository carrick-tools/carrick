/**
 * The members a service's server-framework labels name, for an `infer`
 * request's route rows (carrick#2674).
 *
 * The labels are verified again with the library-claims verifier, which reads
 * them through a probe file. That file is added to a program of its own (the
 * key's `scripts` program, built by the same factory and resolution host),
 * never to the program the inferrer reads: adding and removing it there
 * rebuilt that program twice and threw away its warm checker. Members join
 * across the two programs by `declarationKey`, the compiler's file name and
 * the declaration's start.
 */
import type { Project } from 'ts-morph';
import { type LabelledMembers, type ModuleGraph } from './library-claims.js';
import type { ServerClaims } from './types.js';
/** What the label read needs of the project loader. */
export interface LabelSource extends ModuleGraph {
    getRepoRoot(): string;
    /** A program for `key` the caller owns and drops: never the inferrer's. */
    scriptProjectFor(key: string): Project;
}
/** No label: the inferrer reads nothing new. */
export declare const NO_LABELS: LabelledMembers;
/**
 * The members `claims` name in the program behind `key`. A claim the schema
 * refuses is dropped; a read that throws answers no labels, so every item of
 * the request is read as it would be without them.
 */
export declare function readServerLabels(source: LabelSource, key: string, claims: ServerClaims, budgetMs: number, log: (message: string) => void): LabelledMembers;
