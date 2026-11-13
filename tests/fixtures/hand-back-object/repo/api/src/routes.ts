import express from 'express';
import type { Reply } from './types';

const app = express();
app.use(express.json());

app.get('/v1/replies/latest', (req: unknown, res: { json(value: Reply): void }) => {
  const reply: Reply = { data: { id: 'r-1', name: 'latest' } };
  res.json(reply);
});

app.get('/v1/replies/first', (req: unknown, res: { json(value: Reply): void }) => {
  const reply: Reply = { data: { id: 'r-0', name: 'first' } };
  res.json(reply);
});

app.listen(4100);
