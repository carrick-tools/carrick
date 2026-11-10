export class Ledger {
  total(account: string): number {
    return account.length;
  }
}

export function makeAccount(): string {
  return 'acct';
}
