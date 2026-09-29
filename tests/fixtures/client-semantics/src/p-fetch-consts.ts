declare function configure(o: object): void;
const U1 = "/api/p1";
const U2 = "/api/p2";
const U3 = "/api/p3";
const U4 = "/api/p4";
const U5 = "/api/p5";
const U6 = "/api/p6";
const WRITTEN = { method: "POST" };
WRITTEN.method = "PUT";
const PASSED = { method: "DELETE" };
configure(PASSED);
const CLEAN = { method: "PATCH" };
const NOMETHOD = { headers: { a: "b" } };

export function p1(): unknown { return fetch(U1, WRITTEN); }
export function p2(): unknown { return fetch(U2, PASSED); }
export function p3(): unknown { return fetch(U3, { ...WRITTEN }); }
export function p4(): unknown { return fetch(U4, CLEAN); }
export function p5(): unknown { return fetch(U5, { ...CLEAN }); }
export function p6(): unknown { return fetch(U6, NOMETHOD); }
