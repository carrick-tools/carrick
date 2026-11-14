import type { Request, Reply } from "http-server";

export function getItem(request: Request, reply: Reply): void {
  // One item, by its id.
  reply.send({ id: request.params.id });
}
