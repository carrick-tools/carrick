import type { Request, Reply } from "http-server";

export function allV1(request: Request, reply: Reply): void {
  // Everything under /v1 is answered with its own parameters.
  reply.send({ rest: request.params });
}
