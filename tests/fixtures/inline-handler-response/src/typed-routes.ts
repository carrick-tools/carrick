import { TypedRouter } from '@fixture-http/runtime';
import { listWidgets, summarize } from './widgets';

// The same inline arrangement as `routes.ts`, against a framework whose send
// returns a wrapper carrying the payload's type. The framework is an installed
// dependency here, as it is in production.
export const typedRouter = new TypedRouter();

typedRouter.get('/typed/widgets', async (ctx) => {
  const widgets = await listWidgets();
  return ctx.json(widgets);
});

typedRouter.get('/typed/widgets/summary', async (ctx) => {
  return ctx.json(await summarize());
});
