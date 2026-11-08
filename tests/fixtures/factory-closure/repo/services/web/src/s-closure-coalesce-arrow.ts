import { send } from '@fixture/send';
type Send = (url: string, body?: unknown) => Promise<unknown>;
const make = (o: { token: string; send?: Send }) => {
  const doSend = o.send ?? send;
  return async (p: string) => doSend(`/p${p}`);
};
export function f() { const r = make({ token: 't' }); return r('/s-closure-coalesce-arrow'); }
