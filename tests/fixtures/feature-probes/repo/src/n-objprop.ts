import { http } from '@fixture/http';
const R = { a: '/p/n-objprop' };
export function f() { return http.post(R.a, null); }
