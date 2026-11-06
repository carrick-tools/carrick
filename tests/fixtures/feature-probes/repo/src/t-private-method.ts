import { http } from '@fixture/http';
class Api { #send(p: string) { return http.post(p, null); } go() { return this.#send('/p/t-private-method'); } }
export function f() { return new Api().go(); }
