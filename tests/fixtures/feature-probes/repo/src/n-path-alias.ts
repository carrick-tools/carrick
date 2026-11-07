import { http } from '@fixture/http';
import { P4 } from '@consts/n-path-alias';
export function f() { return http.post(P4, null); }
