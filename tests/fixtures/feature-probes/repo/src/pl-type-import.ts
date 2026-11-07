import { http } from '@fixture/http';
import type { Body } from './consts/pl-types';
declare const body: Body;
export function f() { return http.post('/p/pl-type-import', body); }
