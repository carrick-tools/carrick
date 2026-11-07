// GET /api/v2/orders/:orderId, derived from the file name and the exported
// loader, with no model and no installed dependency.

export type Order = {
  id: string;
  total: number;
};

export async function loader({ params }: { params: { orderId: string } }): Promise<Order> {
  return { id: String(params.orderId), total: 0 };
}
