import { send } from '@fixture/send';
type Send = (url: string, body?: unknown) => Promise<unknown>;
async function quiet(url: string) { return url; }
function make(o: { send?: Send }) {
  const doSend = o.send ?? send;
  async function request(p: string) { return doSend(`/p${p}`); }
  return request;
}
function outer(o: { send?: Send }) { return make(o); }
export function f() { const r = outer({ send: quiet }); return r('/d-nested-factory'); }
