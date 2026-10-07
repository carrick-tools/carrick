import { createEngine, type Ctx } from '@fixture-http/router';

interface RequestWithCtx extends Request {
  ctx: Ctx;
}

const engine = createEngine();

// Returns transport built by something else. What it is handed is the
// request, which is not the body.
export function handleQuery(c: Ctx): Response | Promise<Response> {
  const request = c.req.raw as RequestWithCtx;
  request.ctx = c;
  return engine.handle(request);
}
