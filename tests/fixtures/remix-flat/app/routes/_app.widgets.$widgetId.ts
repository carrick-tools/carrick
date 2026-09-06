// A `_`-prefixed segment is a PATHLESS LAYOUT, not a private file
// (carrick#702): it nests the module without contributing a path segment, so
// this module serves `/widgets/:widgetId`. The global `_`-prefix skip that
// used to hide it was written for a different convention's reserved files, and
// is now convention data (`private_file_prefixes`, empty here).
//
// The page beneath it (`widgets.$widgetId.tsx`) serves the same path from its
// own file, which is a fact and not a clash — both rows are kept. Their loaders
// are declared to the SAME shape on purpose: the type manifest keys a producer
// alias on (method, path) alone, so two producers at one path otherwise race
// for one alias and the fixture's response type flips between scans. That race
// is a real defect (carrick#718); this fixture is an answer key for route
// derivation and should not carry it.

type WidgetView = { title: string };

export const loader = makeLayoutRoute(
  async (): Promise<WidgetView> => ({ title: "Widget" }),
);

declare function makeLayoutRoute(
  handler: () => Promise<WidgetView>,
): unknown;
