import { http } from '@fixture/http';
declare const u: '/n/union-name-a' | '/n/union-name-b';
export function f() { return http.post(u, null); }
