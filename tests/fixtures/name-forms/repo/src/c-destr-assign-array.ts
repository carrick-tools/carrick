import { http } from '@fixture/http';
let path = '';
[path] = ['/c/destr-assign-array'];
export function f() { return http.post(path, null); }
