import { http } from '@fixture/http';
const P = '/p/n-as-cast' as string;
export function f() { return http.post(P, null); }
