import { PAYMENTS_VENDOR_BASE, type Charge } from "./vendor.types.ts";

export async function createCharge(amount: number): Promise<Charge> {
  const res = await fetch(`${PAYMENTS_VENDOR_BASE}/v2/charges`, {
    method: "POST",
    body: JSON.stringify({ amount }),
  });
  return res.json();
}
