import { http } from '@fixture/http';
const enum E { A = '/c/const-enum' }
export function f() { return http.post(E.A, null); }
