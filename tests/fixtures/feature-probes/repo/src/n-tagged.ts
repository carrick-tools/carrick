import { http } from '@fixture/http';
export function f() { return http.post(String.raw`/p/n-tagged`, null); }
