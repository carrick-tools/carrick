import { http } from '@fixture/http';
const K = 'a';
const R = { [K]: '/c/object-written/a' };
export function set(path: string) { R[K] = path; }
export function f() { return http.post(R[K], null); }
