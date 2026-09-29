declare const init: RequestInit;
declare const prod: boolean;
const F1 = "/api/f1";
const F2 = "/api/f2";
const F3 = "/api/f3";
const F4 = "/api/f4";
const F5 = "/api/f5";

export function f1(): unknown { return fetch(F1, { method: "POST", ...init }); }
export function f2(): unknown { return fetch(F2, { ...init, method: "POST" }); }
export function f3(): unknown { return fetch(F3, { ...(prod ? { method: "PUT" } : {}) }); }
export function f4(): unknown { return fetch(F4, { ...init }); }
export function f5(): unknown { return fetch(F5, { headers: { a: "b" } }); }
