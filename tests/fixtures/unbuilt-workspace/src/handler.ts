import { openLedger } from '@acme/ledger';

export function balance(account: string): number {
  const ledger = openLedger();
  return ledger.total(account);
}
