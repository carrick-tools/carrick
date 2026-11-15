// Reads only the refund's id with a plain fetch, so the refunds route has one
// ordinary consumer that a rename of `amount` does not touch.

type RefundRef = { id: string };

export async function refundIdOf(refundId: string): Promise<string> {
  const response = await fetch(`/api/v1/refunds/${refundId}`);
  const refund = (await response.json()) as RefundRef;
  return refund.id;
}
