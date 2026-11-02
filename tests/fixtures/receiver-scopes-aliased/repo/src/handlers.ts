import type { Reply, Request } from 'fixture-aliased-server';

export function ping(request: Request, reply: Reply) {
  reply.send({ ok: true, url: request.url });
}
