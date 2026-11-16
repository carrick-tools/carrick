// GET /api/v1/refunds/:refundId in another service
export type LegacyRefund = { refund_id: number; cents: number };
export async function loader({ params }: { params: { refundId: string } }): Promise<LegacyRefund> {
  return { refund_id: Number(params.refundId), cents: 350 };
}
