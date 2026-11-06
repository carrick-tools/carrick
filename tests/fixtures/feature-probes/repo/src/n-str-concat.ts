import { http } from '@fixture/http';
export function f() { return http.post('/p/'.concat('n-str-concat'), null); }
