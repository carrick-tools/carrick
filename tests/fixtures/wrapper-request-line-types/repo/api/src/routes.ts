import express from 'express';
import type { Order, OrderBody, Refund, RefundBody } from './types';

const app = express();
app.use(express.json());

app.post('/v1/orders', (req: { body: OrderBody }, res: { json(value: Order): void }) => {
  const order: Order = { id: 'o-1', item: req.body.body };
  res.json(order);
});

app.post('/v1/refunds', (req: { body: RefundBody }, res: { json(value: Refund): void }) => {
  const refund: Refund = { id: 'r-1', amount: req.body.body };
  res.json(refund);
});

app.listen(4100);
