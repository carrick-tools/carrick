import type { Response } from "express-shape";

export function listOrders(res: Response, list: string[], total: number): void {
  const orders = list.map((id) => id);
  const count = orders.length;
  void count;
  res.json({ orders, total });
}

export function getOrder(res: Response, id: string, settled: boolean, placedAt: Date): void {
  res.json({ id, settled, placedAt });
}

export function pickOrder(res: Response, flag: boolean): void {
  if (flag) {
    res.json({ n: 1 });
    return;
  }
  res.json({ n: 2 });
}
