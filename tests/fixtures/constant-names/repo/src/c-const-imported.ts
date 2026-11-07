import { http } from '@fixture/http';
import { BASE2 } from './consts';
export function f() { return http.post(`${BASE2}/items`, null); }
