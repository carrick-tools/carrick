interface Req {
  params: Record<string, string>;
}

declare const app: {
  get(path: string, handler: (req: Req, res: Res) => unknown): void;
};

app.get('/orders', (_req: Req) => {
  const body: { id: string; total: number }[] = [];
  return body;
});

interface Res {
  json(body: unknown): void;
  send(body: unknown): void;
  status(code: number): Res;
}

app.get('/items', (_req, res) => {
  res.json({ id: 1 });
});
app.get('/notes', (_req: Req, res: Res) => {
  return res.send({ id: 2 });
});
app.get('/gone', (_req, res) => {
  res.status(404).json({ id: 3 });
});
