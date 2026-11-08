import { send } from '@fixture/send';
function make(o: { token: string }) {
  const doSend = send;
  async function request(p: string) { return doSend(`/p${p}`); }
  return request;
}
export function f() { const r = make({ token: 't' }); return r('/s-closure-local'); }
