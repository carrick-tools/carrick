import { send } from '@fixture/send';
type Send = (url: string, body?: unknown) => Promise<unknown>;
export function make(o: { send?: Send }) {
  const d = o.send ?? send;
  async function check() { return d('/p/z-literal-fn'); }
  return check;
}
export const v = make({});
