// Synthetic sends in the shapes a producer's reply takes. Every name is
// invented; the checks in held-place.test.ts wrap the values below.

declare const res: { json(body?: any): unknown };
declare const c: { json(body?: any): unknown };
declare const order: { id: string; total: number };
declare function load(): { orders: string[] };

export function expressShape(): void {
  res.json({ id: order.id, total: order.total });
}

export function honoShape(): unknown {
  const result = load();
  return c.json(result);
}

export function lambdaShape(payload: { ok: boolean }): { statusCode: number; body: string } {
  return { statusCode: 200, body: JSON.stringify(payload) };
}

export function twoSends(flag: boolean): unknown {
  if (flag) {
    return res.json({ first: 1 });
  }
  return res.json({ second: 2 });
}

export function shorthandShape(): { result: { orders: string[] } } {
  const result = load();
  return { result };
}

export function dateShape(): unknown {
  return res.json({ at: new Date(0), n: 1 });
}
