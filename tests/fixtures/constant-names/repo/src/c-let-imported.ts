import { http } from '@fixture/http';
import { BASE } from './names';
export function f() { return http.post(`${BASE}/items`, null); }
