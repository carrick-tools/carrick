// The page plane shares the directory, the filename grammar AND the export
// names with the API plane — and, under R1b, that is because it is the same
// plane: this module's read handler answers `GET /widgets/:widgetId`, and the
// component it also exports is recorded beside the row as `view_module` rather
// than used to suppress it (carrick#704).
//
// It serves that path alongside the pathless layout in
// `_app.widgets.$widgetId.ts`, and returns a DIFFERENT shape from it on
// purpose: two producers at one path each resolve their own type
// (carrick#718).

export const loader = makePageLoader(async () => ({ title: "Widget" }));

export default function WidgetPage() {
  return null;
}

declare function makePageLoader(handler: () => Promise<unknown>): unknown;
