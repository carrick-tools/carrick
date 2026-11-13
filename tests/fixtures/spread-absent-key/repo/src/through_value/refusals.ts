// A gate run only through a value (`{ ledger: ledgerGate }`), whose `args`
// reach `refusal` through `{ ...args, note }`, written both in the gate's
// own body and in an arrow inside it that reads `args` from the gate.
export type Refuser = { warn(message: string): void; reject(code: number): string };
export type RefusalArgs = { log: Refuser; note?: string };
type Deps = { ready(): boolean };

function refusal({ log, note }: RefusalArgs, reason: string): string {
  return log.reject(reason.length + (note ? 1 : 0));
}

function ledgerGateWith(deps: Deps, args: RefusalArgs): string {
  const { log, note } = args;
  const ledgerNote = `${note || ''}:ledger`;
  if (!deps.ready()) {
    return refusal({ ...args, note: ledgerNote }, 'stopped');
  }
  const unreadable = (cap: string): string => {
    log.warn('meter unavailable');
    return refusal({ ...args, note: ledgerNote }, cap);
  };
  return unreadable('pool');
}

function readySafely(): boolean {
  return true;
}

function ledgerGate(args: RefusalArgs): string {
  return ledgerGateWith({ ready: readySafely }, args);
}

export function gates(): { ledger(args: RefusalArgs): string } {
  return { ledger: ledgerGate };
}
