import { http } from '@fixture/http';
const A = '/n/coalesce-a';
const B = '/n/coalesce-b';
declare const flag: boolean;
export function f() { return http.post(flag ? A : B, null); }
