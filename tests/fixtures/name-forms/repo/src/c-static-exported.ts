import { http } from '@fixture/http';
export class C { static P = '/c/static-exported'; }
export function f() { return http.post(C.P, null); }
