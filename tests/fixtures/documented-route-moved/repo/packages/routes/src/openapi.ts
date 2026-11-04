// A documentation table: handler-less, module-local, each record states its
// responses. It states the operations the handlers in widgets.ts serve; it
// does not serve them.
export const operations = [
  {
    method: 'GET',
    path: '/api/v1/widgets',
    summary: 'List widgets',
    responses: { 200: { description: 'OK' } },
  },
  {
    method: 'POST',
    path: '/api/v1/widgets/{widgetId}/parts',
    summary: 'Add a part to a widget',
    responses: { 201: { description: 'Created' } },
  },
];

export function buildDocument() {
  return { openapi: '3.0.0', operations };
}
