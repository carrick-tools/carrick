import { http } from '@fixture/http';
const R = Object.fromEntries([['a', '/p/n-fromentries']]);
export function f() { return http.post(R.a, null); }
