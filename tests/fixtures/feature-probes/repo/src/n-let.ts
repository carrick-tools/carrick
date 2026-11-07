import { http } from '@fixture/http';
let P = '/p/n-let';
export function f() { return http.post(P, null); }
