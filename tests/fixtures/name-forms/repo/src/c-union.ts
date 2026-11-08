import { http } from '@fixture/http';
declare const u: '/n/union-a' | '/n/union-c';
declare const flag: boolean;
export function f() { return http.post(flag ? u : '/n/union-b', null); }
