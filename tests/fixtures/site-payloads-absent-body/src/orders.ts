import { http } from "@fixture/http";

export interface Order {
  sku: string;
}

export function ping() { return http.post("/orders/ping"); }

function bump(id: string) { return http.post(`/orders/${id}/bump`); }
export function bumpOne() { return bump("1"); }

function send(u: string, body: unknown = {}) { return http.post(u, body); }
export function defaulted() { return send("/orders/defaulted"); }

export function spread(rest: [unknown]) { return http.post("/orders/spread", ...rest); }

export function removeOne() { return http.remove("/orders/1"); }

export function removeTwo() { return http.remove("/orders/2", { headers: {} }); }

export function purge() { return http.purge("/orders/3"); }

export function sendNull() { return http.post("/orders/null", null); }

export function create(order: Order) { return http.post("/orders", order); }

function either(u: string, b?: Order) { return b ? http.post(u, b) : http.post(u); }
export function sendEither(order: Order) { return either("/orders/either", order); }

// A wrapper that writes the body onto the options its caller hands it: the
// caller's literal lacks `data`, yet the request carries one.
function stamp(u: string, init: { headers?: object; data?: unknown }) { init.data = "x"; return http.remove(u, init); }
export function stampNow() { return stamp("/orders/stamp", { headers: {} }); }

// The same through a helper the wrapper hands the options to first.
function addData(init: { headers?: object; data?: unknown }) { init.data = "x"; }
function helped(u: string, init: { headers?: object; data?: unknown }) { addData(init); return http.remove(u, init); }
export function helpedNow() { return helped("/orders/helped", { headers: {} }); }

// carrick#4294: a closed options literal below the deciding call lacks `data`.
function drop(u: string) { return http.remove(u, { headers: {} }); }
export function dropNine() { return drop("/orders/9"); }

function makeRemover(base: string) { return (id: string) => http.remove(`${base}/orders/${id}`, { headers: {} }); }
export const removeMade = makeRemover("/svc");

// Must not be decided: the literal spreads its caller's options.
function dropWith(u: string, extra: { headers?: object; data?: unknown }) { return http.remove(u, { ...extra }); }
export function dropWithData(order: Order) { return dropWith("/orders/with", { data: order }); }

// Must not be decided: the options are built, and a branch may add the body.
function dropBuilt(u: string, b?: Order) {
  const o: { headers?: object; data?: unknown } = { headers: {} };
  if (b) o.data = b;
  return http.remove(u, o);
}
export function dropBuiltNone() { return dropBuilt("/orders/built"); }

// Must not be decided: the literal is handed to a hop that writes the body onto it.
function stampVia(u: string) { return stamp(u, { headers: {} }); }
export function stampLater() { return stampVia("/orders/later"); }
