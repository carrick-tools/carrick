export const documentedRoutes = [
  {
    method: 'GET',
    path: '/api/v1/items/{id}',
    summary: 'Read one item',
    responses: { 200: { schema: { $ref: '#/components/schemas/Item' } } },
  },
  {
    method: 'GET',
    path: '/items/{id}',
    summary: 'Read one item, unversioned',
    responses: { 200: { schema: { $ref: '#/components/schemas/Item' } } },
  },
];
