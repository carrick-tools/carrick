import { http } from '@fixture/http';
declare const P: '/p/n-literal-type';
export function f() { return http.post(P, null); }
