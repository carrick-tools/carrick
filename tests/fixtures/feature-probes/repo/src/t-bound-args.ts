import { http } from '@fixture/http';
function send(p: string) { return http.post(p, null); }
const g = send.bind(null, '/p/t-bound-args');
export function f() { return g(); }
