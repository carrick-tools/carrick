interface Req {
  params: Record<string, string>;
}

declare const app: {
  get(path: string, handler: (req: Req) => unknown): void;
};

app.get('/orders', (_req: Req) => {
  const body: { id: string; total: number }[] = [];
  return body;
});
