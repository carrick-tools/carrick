import { publish } from '@fixture/pub';
type Pub = (t: string, p: unknown) => Promise<void>;
function make(o: { token: string; pub?: Pub }) {
  const pub = o.pub ?? publish;
  async function emit() { return pub('b-closure-coalesce', {}); }
  return emit;
}
export function f() { return make({ token: 't' })(); }
