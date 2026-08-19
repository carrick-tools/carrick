import { LedgerClient, type Invoice, createInvoice } from "ledger-client";

const ledger = new LedgerClient({ region: "eu-west-1" });

export async function charge(amount: number): Promise<void> {
  await ledger.payments.create({ amount });
}

export function draft(): Invoice {
  return createInvoice({ total: 0 });
}
