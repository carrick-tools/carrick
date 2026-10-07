import type { Req, Res } from '@fixture-http/router';

// The module's default export is the handler.
export default async (_req: Req, res: Res) => {
  res.json({ pong: true });
};
