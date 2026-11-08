import { http } from '@fixture/http';
let url: string | undefined = '/c/and-assign';
export function f() { url &&= '/c/and-assign/b'; return http.post(url, null); }
