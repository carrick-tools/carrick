import { http } from '@fixture/http';
const path = () => '/p/n-builder';
export function f() { return http.post(path(), null); }
