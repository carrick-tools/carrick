import type { RouteDef } from '../../../../packages/kit/src/route';

export const pingRoute: RouteDef = {
  method: 'POST',
  path: '/ping/:id',
  handler: (_request, reply) => reply.send({ ok: true }),
};
