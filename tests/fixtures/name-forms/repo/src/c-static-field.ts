import { http } from '@fixture/http';
class C { static P = '/c/static-field'; }
export function f() { return http.post(C.P, null); }
