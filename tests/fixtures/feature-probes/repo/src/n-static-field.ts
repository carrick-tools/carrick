import { http } from '@fixture/http';
class C { static P = '/p/n-static-field'; }
export function f() { return http.post(C.P, null); }
