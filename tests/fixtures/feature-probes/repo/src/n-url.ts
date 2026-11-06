import { http } from '@fixture/http';
export function f() { return http.post(new URL('/p/n-url', 'http://svc.internal').toString(), null); }
