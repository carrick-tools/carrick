import { http } from '@fixture/http';
declare const maybe: string | undefined;
export function f() { return http.post(maybe ?? '/n/coalesce', null); }
