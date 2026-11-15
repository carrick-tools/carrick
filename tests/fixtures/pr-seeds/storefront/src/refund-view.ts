import { getRefund } from "./gen/refunds.gen.js";

export async function amountOf(refundId: string): Promise<number> {
  const { data } = await getRefund({ path: { refundId } });
  return data.amount;
}
