// Which index each part of the hooks reads (carrick#3434). The fact lines
// (part 1) read the fact file, which is the hosted index; the re-check, Read,
// uses and search lines (part 2) read this machine's copy. When the two name
// different commits for a repo, the first part-2 line of a session says so
// once, and the session start says so in its first line. The comparison is
// `generationGap` (`hosted-snapshot.ts`), the one answer; this file reads the
// fact file and keeps the said-once key.

import { createLogger } from "../log.ts";
import { generationGap } from "../hosted-snapshot.ts";
import type { GenerationGap, LocalStamp } from "../hosted-snapshot.ts";
import { commitsOf } from "../facts/peers.ts";
import { openFacts } from "../facts/store.ts";
import type { CheckResult, StatusService } from "../contract.ts";
import { generationSentence } from "../render.ts";
import { markTold, wasTold } from "./reuse.ts";

const log = createLogger("carrick-generation");

/** The stamp one `check` answer carries for its own repo, or null when it names none. */
export function stampOf(result: Pick<CheckResult, "repo" | "index_commit" | "indexed_at">): LocalStamp | null {
  if (!result.repo || !result.index_commit) return null;
  return { repo: result.repo, commit: result.index_commit, ...(result.indexed_at ? { indexedAt: result.indexed_at } : {}) };
}

/** One stamp per repo of `carrick status`: its commit when its services agree on one, else none. */
export function stampsOfStatus(services: readonly Pick<StatusService, "repo" | "index_commit" | "indexed_at">[]): LocalStamp[] {
  const byRepo = new Map<string, Array<Pick<StatusService, "index_commit" | "indexed_at">>>();
  for (const service of services) byRepo.set(service.repo, [...(byRepo.get(service.repo) ?? []), service]);
  return [...byRepo].map(([repo, rows]) => {
    const commits = new Set(rows.map((row) => row.index_commit));
    const commit = commits.size === 1 ? rows[0]!.index_commit : null;
    const indexedAt = rows.find((row) => row.indexed_at)?.indexed_at;
    return { repo, commit, ...(indexedAt ? { indexedAt } : {}) };
  });
}

/** The gaps between the fact file under `root` and `local`, with the fact file's build time. Never throws. */
export async function gapsAgainstFacts(root: string, local: readonly LocalStamp[]): Promise<{ gaps: GenerationGap[]; builtAt: string | null }> {
  if (local.length === 0) return { gaps: [], builtAt: null };
  try {
    const store = await openFacts(root);
    if (store === null) return { gaps: [], builtAt: null };
    try {
      return { gaps: generationGap(commitsOf(store.meta), store.repos, local), builtAt: store.meta["built_at"] ?? null };
    } finally {
      store.close();
    }
  } catch (error) {
    log("generation read failed", String(error));
    return { gaps: [], builtAt: null };
  }
}

/** The session start's sentence: the gap between the fact file and `carrick status`, or null. */
export async function sessionGenerationSentence(root: string, services: readonly StatusService[], now: number): Promise<string | null> {
  const { gaps, builtAt } = await gapsAgainstFacts(root, stampsOfStatus(services));
  return generationSentence(gaps, builtAt, now);
}

/**
 * The sentence for the first part-2 line of a session while the gap stands,
 * from the answers' own stamps (no extra call). Said once per session under
 * `generation::<sorted local commits>`: a new local commit is a new gap.
 */
export async function firstLineGeneration(
  root: string,
  session: string | undefined,
  stamps: readonly LocalStamp[],
  now: number = Date.now(),
  home?: string,
): Promise<string | null> {
  if (!session || stamps.length === 0) return null;
  const { gaps, builtAt } = await gapsAgainstFacts(root, stamps);
  if (gaps.length === 0) return null;
  const key = `generation::${gaps.map((gap) => gap.local).sort().join(",")}`;
  if (wasTold(session, key, home)) return null;
  markTold(session, [key], home);
  return generationSentence(gaps, builtAt, now);
}
