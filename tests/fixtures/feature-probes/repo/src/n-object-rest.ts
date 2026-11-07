import { http } from '@fixture/http';
const { a, ...rest } = { a: '/p/n-object-rest', b: 1 };
export function f() { return http.post(a, rest); }
