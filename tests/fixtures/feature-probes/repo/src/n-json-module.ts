import { http } from '@fixture/http';
import cfg from './consts/routes.json';
export function f() { return http.post(cfg.p, null); }
