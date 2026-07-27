// A framework-private (leading `_`) route file: excluded, like every other
// `_`-prefixed module under a filename-derived convention.

export const loader = makeLayoutRoute(async () => ({}));

declare function makeLayoutRoute(handler: () => Promise<unknown>): unknown;
