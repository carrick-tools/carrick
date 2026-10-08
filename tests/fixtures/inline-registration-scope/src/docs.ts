// The service documents its own routes as data.
export const documentedRoutes = [
  {
    method: 'GET',
    path: '/api/v1/items/{id}',
    summary: 'Read one item',
    responses: { 200: { schema: { $ref: '#/components/schemas/Item' } } },
  },
];
