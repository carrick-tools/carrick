import { http } from '@fixture/http';
const K = 'a';
const R = { [K]: '/c/computed' };
export function f() { return http.post(R[K], null); }
