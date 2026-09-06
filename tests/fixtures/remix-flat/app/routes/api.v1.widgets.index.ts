// A trailing `index` segment collapses onto its parent path, so this module
// and `api.v1.widgets.ts` both serve `GET /api/v1/widgets` from their own
// files. Both rows are kept, and each resolves its own response type
// (carrick#718) — this one returns an empty array where its sibling returns
// declared widgets.

export const loader = makeIndexRoute(async () => []);

declare function makeIndexRoute(handler: () => Promise<unknown[]>): unknown;
