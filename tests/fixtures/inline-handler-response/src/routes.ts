import { requireAuth, Router } from './runtime';
import { createWidget, findWidget, listWidgets, summarize } from './widgets';

// Every route here is registered with its handler passed INLINE: there is no
// named function whose return type states the contract, and the payload is the
// argument of the send, not the handler's return value (which is the transport
// wrapper). carrick#1253.
export const router = new Router();

router.get('/widgets', async (ctx) => {
  const widgets = await listWidgets();
  return ctx.json(widgets);
});

router.get('/widgets/:id', async (ctx) => {
  const widget = await findWidget(ctx.req.param('id'));
  return ctx.json({ widget });
});

router.get('/widgets/summary', async (ctx) => {
  return ctx.json(await summarize());
});

router.post('/widgets', async (ctx) => {
  const body = await ctx.req.json<{ name: string }>();
  try {
    const created = await createWidget(body.name);
    return ctx.json(created, 201);
  } catch (error: unknown) {
    return ctx.json({ error: (error as Error).message }, 500);
  }
});

router.get('/widgets/report', async (ctx) => {
  const widgets = await listWidgets();
  return ctx.json({
    widgets,
    generatedAt: new Date().toISOString(),
  });
});

router.get('/widgets/count', async (ctx) => {
  const widgets = await listWidgets();
  const count = widgets.length;
  return ctx.json({ count });
});

// The handler is not the first argument, and the middleware in front of it
// sends a body of its own. The route's response is the HANDLER's, not the
// gate's rejection.
router.get('/widgets/:id/audit', requireAuth(), async (ctx) => {
  const widget = await findWidget(ctx.req.param('id'));
  return ctx.json({ auditedId: widget.id });
});

// The send is a statement and the handler returns nothing, so the handler's
// return type states no contract at all: what the route serves is only
// readable at the send itself.
router.get('/widgets/legacy', async (ctx) => {
  const widgets = await listWidgets();
  ctx.json(widgets);
});
