import type { FastifyInstance } from 'fastify';

export async function itemRoutes(api: FastifyInstance) {
  api.get('/items/:id', async (request) => {
    const id = (request.params as { id: string }).id;
    return { id, connected: true };
  });

  api.get('/items/:id/status', async (request, reply) => {
    const id = (request.params as { id: string }).id;
    reply.send({ id, state: 'ready' });
  });

  api.get('/items', listItems);
}

function listItems() {
  return [{ id: '1', connected: true }];
}
