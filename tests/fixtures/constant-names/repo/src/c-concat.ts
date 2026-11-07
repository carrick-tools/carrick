import { http } from '@fixture/http';
export function f() { return http.post('/c/'.concat('concat'), null); }
