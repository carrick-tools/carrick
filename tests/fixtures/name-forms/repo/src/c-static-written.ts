import { http } from '@fixture/http';
class C { static P = '/c/static-written'; }
export function reset() { C.P = '/elsewhere'; }
export function f() { return http.post(C.P, null); }
