import { http } from '@fixture/http';
export function f() { return http.post(['', 'p', 'n-join'].join('/'), null); }
