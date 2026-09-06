// A `_`-prefixed segment is a PATHLESS LAYOUT, not a private file
// (carrick#702): it nests the module without contributing a path segment, so
// this module serves `/widgets/:widgetId`. The global `_`-prefix skip that
// used to hide it was written for a different convention's reserved files, and
// is now convention data (`private_file_prefixes`, empty here).
//
// The page beneath it (`widgets.$widgetId.tsx`) serves the same path from its
// own file, which is a fact and not a clash — both rows are kept, and each
// resolves its OWN response type (carrick#718). The two loaders deliberately
// return different shapes so that stays proven.

export const loader = makeLayoutRoute(async () => ({}));

declare function makeLayoutRoute(handler: () => Promise<unknown>): unknown;
