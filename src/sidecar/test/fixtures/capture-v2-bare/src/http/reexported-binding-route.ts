// carrick#766 shape: a route whose handlers are built by a framework factory
// and re-exported as bindings at the bottom of the file. Both operations on
// the file anchor at the EXPORT line, which declares no handler of its own,
// and `fakelib` is pinned-but-absent (the bare-checkout shape, #349), so every
// value flowing through the factory is error-typed.
//
// The capture's infer anchor therefore resolves a node whose type is a bare
// `any`. Publishing that as the route's response contract states "a type was
// inferred and it collapsed", which is a claim the scan cannot back — the
// honest answer is `unknown` plus the reason.
import { createActionRoute, createLoaderRoute } from 'fakelib';

const { action } = createActionRoute({ body: 'CreateBatch' }, async () => ({
  id: 'batch_1',
}));

const loader = createLoaderRoute({ searchParams: 'ListBatches' }, async () => ({
  items: [] as string[],
}));

export { action, loader };

// A private helper inside the line tolerance of the export above: the
// neighbouring declaration carrick#771 stopped the v1 walk reading.
export interface BatchFilter {
  statuses: string[];
  from: number;
}

export function filterToQuery(filter: BatchFilter): BatchFilter {
  return filter;
}

// Clean control, same file and same capture: a concrete local binding an infer
// anchor resolves with no help from the missing dependency.
export const acceptedEnvelope = { batchId: 'batch_1' };

// A LOCATED payload that decays through the same missing dependency. The
// anchor for it names the expression, so whatever type it turns out to have is
// a fact about the payload — including a whole `any` — and the capture keeps
// publishing it. The #766 abstain must not reach this shape.
declare function respond(body: unknown): void;

export async function sendBatchSummary(): Promise<void> {
  const summary = await createLoaderRoute('summary');
  respond(summary);
}
