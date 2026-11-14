import { http } from '@fixture/http';
function send(p: string, b: unknown) { return http.post(p, b); }
function get(p: string) { return fetch(p); }
export function h() { return [send('/v/a', 1), get('/v/b')]; }
