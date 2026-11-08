import { http } from '@fixture/http';
let path = '';
({ path = '/c/destr-assign-default' } = {} as { path?: string });
export function f() { return http.post(path, null); }
