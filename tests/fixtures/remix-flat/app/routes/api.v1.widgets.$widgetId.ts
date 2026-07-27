// The read half of the same shape: a call-expression export named for the
// read role, which serves GET.

import { makeReadRoute } from "~/services/routeBuilders/apiBuilder.server";

type Widget = { id: string; name: string };

export const loader = makeReadRoute({}, async ({ params }): Promise<Widget> => {
  return { id: String(params.widgetId), name: "hex-key" };
});

// Not a handler — a route module may export helpers and config alongside its
// handlers, and those must never become endpoints.
export const config = { maxDuration: 30 };
export function serializeWidget(widget: Widget): string {
  return JSON.stringify(widget);
}
