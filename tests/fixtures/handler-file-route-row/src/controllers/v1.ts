import type { Request, Reply } from "http-server";

export function allV1(request: Request, reply: Reply): void {
  // Everything under /v1 is passed on upstream.
  reply.forward(request);
}
