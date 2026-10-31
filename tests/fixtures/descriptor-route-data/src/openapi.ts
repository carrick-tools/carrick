// A documentation table: module-local, handler-less, each record states its
// responses, and the document builder below reads it in this same file.
const operations = [
  {
    method: 'GET',
    path: '/invoices',
    responses: { 200: { description: 'OK' } },
  },
  {
    method: 'GET',
    path: '/invoices/:id',
    parameters: [{ name: 'id', in: 'path' }],
    responses: { 200: { description: 'OK' } },
  },
  {
    method: 'DELETE',
    path: '/invoices/:id',
    summary: 'Remove one',
    responses: { 204: { description: 'Gone' } },
  },
];

export function buildDocument() {
  return { openapi: '3.0.0', operations };
}
