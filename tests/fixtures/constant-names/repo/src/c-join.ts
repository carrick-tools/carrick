import { http } from '@fixture/http';
export function f() { return http.post(['', 'c', 'join'].join('/'), null); }
