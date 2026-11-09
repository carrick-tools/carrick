import { Ledger } from '@acme/ledger';

export class Handler {
  constructor(private readonly dep: Ledger) {}

  run(account: string): number {
    return this.dep.total(account);
  }
}
