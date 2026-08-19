import { ledger } from "@fixture/ledger-kit/client";

export const charge = async (amount: number): Promise<void> => {
  await ledger.payments.create({ amount });
};
