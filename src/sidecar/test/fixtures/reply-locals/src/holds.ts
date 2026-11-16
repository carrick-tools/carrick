import make from 'reply-desk';
import type { Ctx, Reply, Request } from 'reply-desk';

interface SessionLocals { holder: string; account: string; environment: string; plan: string }
interface ParsedLocals { parsedParams: { key: string }; parsedBody: { note: string } }
interface HoldBody { holder: string; key: string }

const app = make();

// release
function releaseHold(_req: Request, res: Reply<SessionLocals & ParsedLocals>) {
  const { parsedParams, parsedBody } = res.locals;
  res.json({ released: parsedParams.key + parsedBody.note });
}
app.post('/release', releaseHold);

// take
function takeHold(req: Request<HoldBody>, res: Reply) {
  const body = req.body;
  res.json({ taken: body.key });
}
app.post('/take', takeHold);

// context
function contextHold(c: Ctx<HoldBody>) {
  const body = c.body;
  c.json({ held: body.holder });
}
app.put('/context', contextHold);

// helper
function readHold(c: Ctx<HoldBody>) {
  const body = c.body;
  return body.holder;
}
app.put('/helper', (c) => {
  c.json({ held: readHold(c as Ctx<HoldBody>) });
});
