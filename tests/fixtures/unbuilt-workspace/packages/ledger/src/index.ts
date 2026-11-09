export class Ledger {
  total(account: string): number {
    return account.length;
  }
}

export function openLedger(): Ledger {
  return new Ledger();
}
