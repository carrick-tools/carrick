import { http } from '@fixture/http';
let P = '/c/let-reassigned/a';
export function use(b: boolean) { if (b) P = '/c/let-reassigned/b'; }
export function f() { return http.post(P, null); }
