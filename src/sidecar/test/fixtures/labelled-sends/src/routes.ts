import make from 'reply-kit';
import type { Reply } from 'reply-kit';

interface Ok { id: string; total: number }
interface Doc { id: string; title: string }
interface Problem { error: string; code: number }
interface Chunk { seq: number; data: string }
type Found = { kind: 'missing' } | { kind: 'found'; doc: Doc };

declare function check(body: unknown): Promise<boolean>;
declare function ok(): Ok;
declare function load(): Promise<{ failure: Problem | null; doc: Doc }>;
declare function find(body: unknown): Promise<Found>;
declare function start(body: unknown): Promise<string | null>;
declare function stream(): Promise<Chunk[] | null>;
declare const trace: string;

// The repo's own reply type: its members share the library's names, and no
// label names its declarations.
interface OwnReply {
  statusCode: number;
  setHeader(name: string, value: string): OwnReply;
  getHeader(name: string): string | undefined;
  status(code: number): OwnReply;
  json(body: unknown): OwnReply;
  cookie(name: string, value: unknown): OwnReply;
}
declare function own(res: Reply): OwnReply;

const app = make();

// A
app.post('/a', async (req, res) => {
  const bad = await check(req.body);
  res.header('x-trace', trace);
  if (bad) {
    res.status(400).json({ error: 'bad', code: 1 });
    return;
  }
  res.json(ok());
});

// B
app.get('/b', async (req, res) => {
  const { failure, doc } = await load();
  if (failure) {
    res.status(500).json(failure);
  } else {
    res.json(doc);
  }
  return;
});

// C
app.post('/c', async (req, res) => {
  const found = await find(req.body);
  switch (found.kind) {
    case 'missing':
      res.status(404).json({ missing: true });
      break;
    case 'found':
      res.json(found.doc);
      break;
  }
});

// D
app.post('/d', async (req, res) => {
  const id = await start(req.body);
  if (!id) {
    res.status(400).json({ error: 'no session', code: 2 });
    return;
  }
  res.json(ok());
  res.cookie('session', { id });
});

// E
app.post('/e', async (req, raw) => {
  const res = own(raw);
  const id = await start(req.body);
  if (!id) {
    res.status(400).json({ error: 'no session', code: 2 });
    return;
  }
  res.json(ok());
  res.cookie('session', { id });
});

// G
app.get('/g', async (req, res) => {
  const chunks = await stream();
  if (!chunks) {
    res.status(404).json({ gone: true });
    return;
  }
  for (const c of chunks) res.write(c);
  res.end();
});

// H
app.post('/h', async (req, res) => {
  const bad = await check(req.body);
  if (bad) {
    res.status(400).json({ error: 'bad', code: 3 });
    return;
  }
  res.end();
});
