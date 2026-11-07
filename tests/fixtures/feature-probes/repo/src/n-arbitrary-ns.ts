import { http } from '@fixture/http';
import { 'route-a' as RA } from './consts/n-arbitrary';
export function f() { return http.post(RA, null); }
