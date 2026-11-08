import { send } from '@fixture/send';
type Send = (url: string, body?: unknown) => Promise<unknown>;
function make(post: Send) {
  async function request(p: string) { return post(`/p${p}`); }
  return request;
}
export function f() { const r = make(send); return r('/s-closure-param'); }
