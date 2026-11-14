// Prix: 5 € — the accent above sits before every value on the lines below.
export async function createOrder(sku: string): Promise<unknown> {
  const tag = '😀'; void tag; const res = await fetch('/orders', { method: 'POST', body: JSON.stringify({ sku, qty: 1 }) });
  return res.json();
}

export async function listOrders(): Promise<unknown> {
  const res = await fetch('/orders');
  return res.json();
}
