import { http } from '@fixture/http';
export function f() { return http.post('/p/pl-js-inferred', { a: 1, b: [1, 2] }); }
