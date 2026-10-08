// The service documents its routes as data. The 2xx schema is a reference
// by name, which nothing here resolves, and no handler is named.
export const routeDocs = [
  {
    method: 'GET',
    path: '/status',
    summary: 'Service status',
    tags: ['status'],
    responses: {
      200: {
        description: 'OK',
        content: {
          'application/json': {
            schema: { $ref: '#/components/schemas/StatusDto' },
          },
        },
      },
    },
  },
];
