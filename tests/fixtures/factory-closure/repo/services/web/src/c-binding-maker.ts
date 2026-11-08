import { send } from '@fixture/send';
type Send = (url: string, body?: unknown) => Promise<unknown>;
async function quiet(url: string) { return url; }
function make(o: { send?: Send }) {
  const doSend = o.send ?? send;
  async function request(p: string) { return doSend(`/p${p}`); }
  return request;
}
let client: (p: string) => Promise<unknown>;
export function init(o: { send?: Send }) { client = make(o); }
export function use(o: { send?: Send }) { return client(`/c-binding-maker/${o.send ? 'a' : 'b'}`); }
export function boot() { init({ send: quiet }); return use({ send }); }
