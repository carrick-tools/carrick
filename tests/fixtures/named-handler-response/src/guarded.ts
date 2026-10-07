import type { Next, Req, Res } from '@fixture-http/router';

// Wraps a handler so a rejection reaches the router's error path.
export function guarded<B>(handler: (req: Req<B>, res: Res) => Promise<void>) {
  return (req: Req<B>, res: Res, next: Next) => handler(req, res).catch(next);
}
