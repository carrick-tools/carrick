import { http } from '@fixture/http';
class Api { static send(p: string) { return http.post(p, null); } }
export function f() { return Api.send('/p/t-static-method'); }
