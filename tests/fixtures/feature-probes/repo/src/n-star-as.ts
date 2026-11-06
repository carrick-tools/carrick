import { http } from '@fixture/http';
import { C } from './consts/n-star-as-barrel';
export function f() { return http.post(C.P3, null); }
