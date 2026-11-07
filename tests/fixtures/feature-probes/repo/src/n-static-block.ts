import { http } from '@fixture/http';
class C { static P: string; static { C.P = '/p/n-static-block'; } }
export function f() { return http.post(C.P, null); }
