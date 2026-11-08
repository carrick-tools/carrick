import { http } from '@fixture/http';
const Api = class Named { send(p: string) { return http.post(p, null); } };
export function f() { return new Api().send('/p/t-class-expr-named'); }
