import { http } from '@fixture/http';
const K = 'a';
const R = { [K]: '/p/n-computed' };
export function f() { return http.post(R[K], null); }
