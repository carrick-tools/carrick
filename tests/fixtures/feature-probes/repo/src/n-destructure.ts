import { http } from '@fixture/http';
const { a } = { a: '/p/n-destructure' };
export function f() { return http.post(a, null); }
