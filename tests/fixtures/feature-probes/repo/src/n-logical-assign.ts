import { http } from '@fixture/http';
let url: string | undefined;
export function f() { url ??= '/p/n-logical-assign'; return http.post(url, null); }
