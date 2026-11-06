import { http } from '@fixture/http';
import { P } from './consts/n-imported';
export function f() { return http.post(P, null); }
