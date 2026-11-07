import { http } from '@fixture/http';
import { HOST2 } from './consts';
export function f() { return http.post(`${HOST2}/const-host`, null); }
