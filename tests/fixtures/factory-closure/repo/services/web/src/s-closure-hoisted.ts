import { send } from '@fixture/send';
type Send = (url: string, body?: unknown) => Promise<unknown>;
function make(post: Send) {
  return request;
  async function request(p: string) { return post(`/p${p}`); }
}
export function f() { const r = make(send); return r('/s-closure-hoisted'); }
