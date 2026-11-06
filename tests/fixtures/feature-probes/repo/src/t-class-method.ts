import { http } from '@fixture/http';
class Api { send(p: string) { return http.post(p, null); } }
export function f() { return new Api().send('/p/t-class-method'); }
