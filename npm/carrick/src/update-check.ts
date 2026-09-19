// The detached child that asks the registry what is published.
//
// It exists as its own entry point so that the command a person waits on never
// waits on a network call: `scheduleUpdateCheck` (src/update.ts) starts this,
// unrefs it, and returns. Whatever this learns is read by the NEXT invocation.
//
// It prints nothing, on either stream, in any case. Its parent is already gone
// and its stdio is `ignore`; the only thing it can usefully do with a failure
// is leave the cache alone.

import { fetchLatest, readUpdateState, writeUpdateState } from "./update.ts";

async function main(): Promise<void> {
  const previous = readUpdateState();
  const latest = await fetchLatest();
  // A fetch that answered nothing keeps the last answer rather than erasing it:
  // one flaky minute must not make a machine forget that it is three releases
  // behind. The timestamp moves either way, which is what stops a dead network
  // from being re-dialled on every invocation.
  writeUpdateState({
    checked_at: new Date().toISOString(),
    latest: latest ?? previous?.latest ?? null,
  });
}

await main().catch(() => {});
process.exit(0);
