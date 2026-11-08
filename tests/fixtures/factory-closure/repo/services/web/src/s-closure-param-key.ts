import { send } from '@fixture/send';
type Send = (url: string, body?: unknown) => Promise<unknown>;
function make(o: { post: Send }) {
  async function request(p: string) { return o.post(`/p${p}`); }
  return request;
}
export function f() { const r = make({ post: send }); return r('/s-closure-param-key'); }
