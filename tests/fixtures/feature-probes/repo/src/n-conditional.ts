import { http } from '@fixture/http';
declare const id: string;
declare const flag: boolean;
export function f() { return http.post(flag ? '/p/n-conditional-a' : '/p/n-conditional-b', null); }
