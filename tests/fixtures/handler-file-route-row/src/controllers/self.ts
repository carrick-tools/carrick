import type { Request, Reply } from "http-server";
import { router } from "../router";

export function h(request: Request, reply: Reply): void {
  reply.send({ from: request.params.id });
}

router.get("/a", h);
