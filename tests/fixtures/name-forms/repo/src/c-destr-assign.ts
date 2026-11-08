import { http } from '@fixture/http';
let path = '';
({ path } = { path: '/c/destr-assign' });
export function f() { return http.post(path, null); }
