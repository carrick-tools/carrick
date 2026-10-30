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

import path from 'node:path';
import type { Project } from 'ts-morph';
import { LibraryClaimsVerifier, type LabelledMembers, type ModuleGraph } from './library-claims.js';
import type { ServerClaims } from './types.js';
import { settledServerChecks } from './validators.js';

/** What the label read needs of the project loader. */
export interface LabelSource extends ModuleGraph {
  getRepoRoot(): string;
  /** A program for `key` the caller owns and drops: never the inferrer's. */
  scriptProjectFor(key: string): Project;
}

/** No label: the inferrer reads nothing new. */
export const NO_LABELS: LabelledMembers = { sends: new Map(), sets: new Map(), returns: new Map(), bodies: new Set() };

/**
 * The members `claims` name in the program behind `key`. A claim the schema
 * refuses is dropped; a read that throws answers no labels, so every item of
 * the request is read as it would be without them.
 */
export function readServerLabels(
  source: LabelSource,
  key: string,
  claims: ServerClaims,
  budgetMs: number,
  log: (message: string) => void
): LabelledMembers {
  const { checks, refused } = settledServerChecks(claims.checks);
  if (refused > 0) {
    log(`Dropped ${refused} server claim(s) the library-check schema refuses (a make or scope among them leaves its ops unread)`);
  }
  if (checks.length === 0) return NO_LABELS;
  const fromDir = path.resolve(source.getRepoRoot(), claims.from_dir);
  try {
    const verifier = new LibraryClaimsVerifier(source.scriptProjectFor(key), {
      packageDirectoryOf: (file) => source.packageDirectoryOf(file),
      resolveModule: (name, from, options) => source.resolveModule(name, from, options),
    });
    const labels = verifier.labelledMembers(fromDir, checks, budgetMs, claims.scan_root, claims.type_packages);
    log(
      `Read ${labels.sends.size} labelled send, ${labels.sets.size} assigned reply, ${labels.returns.size} returned reply and ${labels.bodies.size} body member(s) for handler parameters ` +
        `from ${checks.length} claim(s) in ${fromDir}: ${JSON.stringify(labels.outcomes ?? {})}`
    );
    return labels;
  } catch (error) {
    log(`Reading server labels in ${fromDir} failed, so no handler parameter is read through them: ${String(error)}`);
    return NO_LABELS;
  }
}
