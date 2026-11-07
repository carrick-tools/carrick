import { http } from '@fixture/http';
import { ROUTES } from './consts/n-routes';
export function f() { return http.post(ROUTES.a, null); }
