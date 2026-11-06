import { http } from '@fixture/http';
interface Body { a: number }
export function f() { return http.post('/p/pl-satisfies', { a: 1 } satisfies Body); }
