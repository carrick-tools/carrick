import { http } from '@fixture/http';
declare const flag: boolean;
let p = '/c/let-written/a';
export function f() { if (flag) p = '/c/let-written/b'; return http.post(p, null); }
