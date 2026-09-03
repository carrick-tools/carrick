// The response contract the ledger client reads. Declared here and imported at
// the call site, so the manifest's own file/line is the USE and this file is the
// declaration.
export interface LedgerEntry {
  id: string;
  amount: number;
}
