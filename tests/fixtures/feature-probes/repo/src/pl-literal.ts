import { http } from '@fixture/http';
export function f() { return http.post('/p/pl-literal', { a: 1, b: 'x' }); }
