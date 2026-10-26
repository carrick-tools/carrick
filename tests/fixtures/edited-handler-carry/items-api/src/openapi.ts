// The service documents its routes as data. Each 2xx schema is a reference by
// name, so the descriptor types nothing and the handler carries the type.
export const itemDocs = [
  {
    method: 'GET',
    path: '/api/v1/items/{id}',
    summary: 'One item',
    responses: {
      200: {
        description: 'OK',
        content: {
          'application/json': {
            schema: { $ref: '#/components/schemas/Item' },
          },
        },
      },
    },
  },
  {
    method: 'GET',
    path: '/api/v1/items/{id}/status',
    summary: 'One item status',
    responses: {
      200: {
        description: 'OK',
        content: {
          'application/json': {
            schema: { $ref: '#/components/schemas/Item' },
          },
        },
      },
    },
  },
  {
    method: 'GET',
    path: '/api/v1/items',
    summary: 'All items',
    responses: {
      200: {
        description: 'OK',
        content: {
          'application/json': {
            schema: { $ref: '#/components/schemas/Item' },
          },
        },
      },
    },
  },
];
