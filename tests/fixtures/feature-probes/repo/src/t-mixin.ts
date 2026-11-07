import { http } from '@fixture/http';
const M = { send(p: string) { return http.post(p, null); } };
class Api {}
interface Api { send(p: string): Promise<unknown>; }
Object.assign(Api.prototype, M);
export function f() { return new Api().send('/p/t-mixin'); }
