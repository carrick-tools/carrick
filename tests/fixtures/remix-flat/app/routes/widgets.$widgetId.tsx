// The UI page plane shares the directory, the filename grammar AND the export
// names with the API plane. Excluding `.tsx` is what keeps every page in the
// app out of the endpoint index.

export const loader = makePageLoader(async () => ({ title: "Widget" }));

export default function WidgetPage() {
  return null;
}

declare function makePageLoader(handler: () => Promise<unknown>): unknown;
