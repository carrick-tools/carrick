import { http } from '@fixture/http';
let url = '';
export function f() { url ||= '/c/or-assign'; return http.post(url, null); }
