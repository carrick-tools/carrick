import { http } from '@fixture/http';
const M = new Map([['k', '/p/n-map']]);
export function f() { return http.post(M.get('k')!, null); }
