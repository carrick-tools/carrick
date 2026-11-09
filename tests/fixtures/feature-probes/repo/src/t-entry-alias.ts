import { http } from '@fixture/http';
const api = { send: (p: string) => http.post(p, null) };
const g = api.send;
export function f() { return g('/p/t-entry-alias'); }
