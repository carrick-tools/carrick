// POST /api/v2/orders, derived from the file name and the exported action.

export type OrderDraft = {
  sku: string;
  quantity: number;
};

export type Order = {
  id: string;
  total: number;
};

export async function action({ request }: { request: Request }): Promise<Order> {
  const draft = (await request.json()) as OrderDraft;
  return { id: draft.sku, total: draft.quantity };
}
