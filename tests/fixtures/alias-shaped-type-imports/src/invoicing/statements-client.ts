import type { Statement } from '@invoicing/contracts/statement';
import type { Ledger } from '@invoicing/contracts/ledger';

export async function getStatement(number: string): Promise<Statement> {
  const response = await fetch(`/statements/${number}`);
  return response.json() as Promise<Statement>;
}

export async function getLedger(number: string): Promise<Ledger> {
  const response = await fetch(`/ledgers/${number}`);
  return response.json() as Promise<Ledger>;
}
