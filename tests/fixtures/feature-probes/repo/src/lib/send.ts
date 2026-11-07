import { http } from '@fixture/http';
export function send(p: string) { return http.post(p, null); }
