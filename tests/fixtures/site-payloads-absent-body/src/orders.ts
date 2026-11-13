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
