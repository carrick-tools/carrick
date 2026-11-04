import type { Channel, Deps, Notifier, Orphan, Procedures, Reader, Store, Transport } from "./ports.js";
import { makeStore } from "./store.js";

export function viaAnnotated(store: Store) {
  return store.read("a");
}

export function viaDestructured({ store }: Deps) {
  store.write("a", 1);
}

export function viaFactory() {
  const s = makeStore();
  s.remove("a");
}

export function viaChain(holder: { deps: Deps }) {
  holder.deps.store.read("b");
}

let current: Store | null = null;
export function viaUnion() {
  current?.read("c");
}

export function viaImplements(t: Transport) {
  return t.send("x");
}

export function viaAbstract(c: Channel) {
  c.open();
}

export function viaArrowEntry(p: Procedures) {
  return p.getUser("1");
}

export function viaFunctionProperty(d: Deps) {
  d.publish("t");
}

export function viaSeveral(n: Notifier) {
  n.notify("e");
}

export function viaBinding(r: Reader) {
  return r.get();
}

export function viaOrphan(o: Orphan) {
  o.ping();
}

export function viaAny(x: any) {
  x.read();
}
