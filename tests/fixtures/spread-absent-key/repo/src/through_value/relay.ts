// Two spreads on the way up: `leaf` reads `log` from `mid`'s
// `{ ...opts }`, and `mid` is called with `{ ...args }`, `args` the
// parameter of a relay run only through a value. Before the rule the read
// stopped at the spread nearest the call, one unread write; it must still
// read as that one write, as the twin `gates.ts:35` does.
import type { GateArgs } from './gates';

function leaf({ log }: GateArgs): number {
  log.warn('relayed');
  return 4;
}

function mid(opts: GateArgs): number {
  return leaf({ ...opts });
}

function relay(args: GateArgs): number {
  return mid({ ...args });
}

export const relays = { relay };
