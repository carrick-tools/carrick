import { http } from '@fixture/http';
export function f() { return http.post('/c/replace/:kind'.replace(':kind', 'all'), null); }
