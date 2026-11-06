import { http } from '@fixture/http';
const api = { send(p: string) { return http.post(p, null); } };
export function f() { return api.send('/p/t-object-method'); }
