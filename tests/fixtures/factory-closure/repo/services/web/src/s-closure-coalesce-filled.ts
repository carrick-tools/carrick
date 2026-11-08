import { send } from '@fixture/send';
type Send = (url: string, body?: unknown) => Promise<unknown>;
async function quiet(url: string) { return url; }
function make(o: { token: string; send?: Send }) {
  const doSend = o.send ?? send;
  async function request(p: string) { return doSend(`/p${p}`); }
  return request;
}
export function f() { const r = make({ token: 't', send: quiet }); return r('/s-closure-coalesce-filled'); }
