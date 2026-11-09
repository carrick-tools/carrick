import { Ledger, makeAccount } from '@acme/ledger';
import { readFlag } from '@acme/flags';
import { part } from '@acme/split';

export class Handler {
  constructor(private readonly dep: Ledger) {}

  run(): number {
    const account = makeAccount();
    const flag = readFlag();
    const piece = part();
    return this.dep.total(account) + (flag.enabled ? 1 : 0) + Number(piece.size);
  }
}

export function currentFlag() {
  return readFlag();
}

export type CurrentFlag = ReturnType<typeof currentFlag>;
