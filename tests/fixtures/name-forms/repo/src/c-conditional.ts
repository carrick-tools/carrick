import { http } from '@fixture/http';
declare const flag: boolean;
export function f() { return http.post(flag ? '/n/conditional-a' : '/n/conditional-b', null); }
