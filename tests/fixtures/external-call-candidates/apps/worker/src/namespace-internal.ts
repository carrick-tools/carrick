import * as ledgerKit from "@fixture/ledger-kit";

export const settle = async (id: string): Promise<void> => {
  await ledgerKit.ledger.payments.settle(id);
};
