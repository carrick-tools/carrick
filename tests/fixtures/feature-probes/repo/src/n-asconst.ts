import { http } from '@fixture/http';
const R = { a: '/p/n-asconst' } as const;
export function f() { return http.post(R.a, null); }
