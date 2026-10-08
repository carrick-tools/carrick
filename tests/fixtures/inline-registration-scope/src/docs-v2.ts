export const documentedRoutes = [
  {
    method: 'GET',
    path: '/v2/items/{id}',
    summary: 'Read one item',
    responses: { 200: { schema: { $ref: '#/components/schemas/Item' } } },
  },
];
