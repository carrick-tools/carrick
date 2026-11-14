import type { Request, Reply } from "http-server";

export function gql(request: Request, reply: Reply): void {
  reply.send({ data: request.params.query });
}
