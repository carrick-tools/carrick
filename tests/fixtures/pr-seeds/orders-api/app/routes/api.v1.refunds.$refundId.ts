// GET /api/v1/refunds/:refundId, from the file name and the exported loader.
export type Refund = {
  id: string;
  amount: number;
  reason: string;
};

export async function loader({
  params,
}: {
  params: { refundId: string };
}): Promise<Refund> {
  return { id: String(params.refundId), amount: 350, reason: "damaged" };
}
