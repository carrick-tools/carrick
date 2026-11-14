import { http } from '@fixture/http';
const PREFIX = '/api';
export function f() { return http.post(`${PREFIX}/p/one`, null); }
