import { createRouter } from '@fixture-http/router';

// A registration bound to a name. Its value is the router, not a handler.
export const innerRouter = createRouter().get('/items', (_req, res) => {
  res.json({ inner: true });
});
