import type { LedgerRow } from '../generated/ledger-client';

export interface Ledger {
  number: string;
  memo: unknown;
  row: LedgerRow;
}
