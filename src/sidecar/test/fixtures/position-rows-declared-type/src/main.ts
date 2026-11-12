import { createProxy, createHandle, createPlain, type ProxyOf, type Router } from "proxy-lib";
import { createHooks } from "hooks-lib";

type AppRouter = { record: { a: number; b: string } } & Router;

// An in-repo alias of the package alias, then of that alias.
type Inner = ProxyOf<AppRouter>;
type Runner = Inner;

class Worker {
  client: Runner = createProxy<AppRouter>();
  other: Runner;
  constructor(other: Runner) {
    this.other = other;
  }
  run() {
    return this.client.a.query();
  }
  again() {
    return this.other.b.query();
  }
}

// A make's declared return, held in a const with no annotation.
const made = createProxy<AppRouter>();
made.a.query();

// A package alias that is a plain object type.
const handle = createHandle<AppRouter>();
handle.call();

// A make's declared return that is an intersection alias.
const hooks = createHooks<{ x: number }>();
hooks.x.use();

// An interface: no row (widening to interfaces is a deliberate, measured change).
const plain = createPlain();
plain.query();

// An in-repo alias of a local type: nothing package-declared on the chain.
type Local = { query(): void };
const local: Local = { query() {} };
local.query();

// An annotation of a union with the package alias in it: the first package reference.
function takes(p: ProxyOf<AppRouter> | undefined) {
  return p?.a.query();
}
takes(made);
