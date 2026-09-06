// A trailing `index` segment collapses onto its parent path, so this module
// and `api.v1.widgets.ts` both serve `GET /api/v1/widgets` from their own
// files. Both rows are kept, and their loaders are declared to the same shape
// for the reason spelled out in `_app.widgets.$widgetId.ts` (carrick#718).

type Widget = { id: string; name: string };

export const loader = makeIndexRoute(async (): Promise<Widget[]> => []);

declare function makeIndexRoute(
  handler: () => Promise<Widget[]>,
): unknown;
