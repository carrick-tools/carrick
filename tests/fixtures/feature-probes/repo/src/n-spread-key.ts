import { http } from '@fixture/http';
const base = { a: '/x' };
const R = { ...base, a: '/p/n-spread-key' };
export function f() { return http.post(R.a, null); }
