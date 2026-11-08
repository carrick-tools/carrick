import { http } from '@fixture/http';
const { path } = { path: '/c/destructure' };
export function f() { return http.post(path, null); }
