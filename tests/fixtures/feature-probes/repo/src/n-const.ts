import { http } from '@fixture/http';
const P = '/p/n-const';
export function f() { return http.post(P, null); }
