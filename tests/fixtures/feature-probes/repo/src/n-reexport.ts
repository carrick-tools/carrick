import { http } from '@fixture/http';
import { P2 } from './consts/n-reexport-barrel';
export function f() { return http.post(P2, null); }
