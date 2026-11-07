import { http } from '@fixture/http';
const R = { a: '/p/n-satisfies' } satisfies Record<string, string>;
export function f() { return http.post(R.a, null); }
