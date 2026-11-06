import { http } from '@fixture/http';
enum E { A = '/p/n-enum' }
export function f() { return http.post(E.A, null); }
