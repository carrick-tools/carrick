import { http } from '@fixture/http';
let P = '/c/let';
export function f() { return http.post(P, null); }
