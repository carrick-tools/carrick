// The page plane shares the directory, the filename grammar AND the export
// names with the API plane — and, under R1b, that is because it is the same
// plane: this module's read handler answers `GET /widgets/:widgetId`, and the
// component it also exports is recorded beside the row as `view_module` rather
// than used to suppress it (carrick#704).
//
// It serves that path alongside the pathless layout in
// `_app.widgets.$widgetId.ts`, which is a fact and not a clash — both rows are
// kept. The two loaders declare the SAME type, explicitly, for the reason that
// file's header gives: a producer's manifest alias is keyed on (method, path)
// alone, so two producers at one path race for it (carrick#718).

type WidgetView = { title: string };

export const loader = makePageLoader(
  async (): Promise<WidgetView> => ({ title: "Widget" }),
);

export default function WidgetPage() {
  return null;
}

declare function makePageLoader(handler: () => Promise<WidgetView>): unknown;
