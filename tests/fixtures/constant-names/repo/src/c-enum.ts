import { http } from '@fixture/http';
enum E { A = '/c/enum' }
export function f() { return http.post(E.A, null); }
