// A trailing `index` segment collapses onto its parent path.

export const loader = makeIndexRoute(async () => []);

declare function makeIndexRoute(handler: () => Promise<unknown[]>): unknown;
