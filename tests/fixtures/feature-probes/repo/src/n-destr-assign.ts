import { http } from '@fixture/http';
let a = '';
({ a } = { a: '/p/n-destr-assign' });
export function f() { return http.post(a, null); }
