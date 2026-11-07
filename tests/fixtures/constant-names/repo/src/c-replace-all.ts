import { http } from '@fixture/http';
export function f() { return http.post('/c/x/x'.replaceAll('x', 'replace-all'), null); }
