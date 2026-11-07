import { http } from '@fixture/http';
export function f() { for (const v of ['a', 'b'] as const) { http.post(`/p/n-loop/${v}`, null); } }
