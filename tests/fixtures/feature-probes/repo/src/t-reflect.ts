import { http } from '@fixture/http';
function send(p: string) { return http.post(p, null); }
export function f() { return Reflect.apply(send, null, ['/p/t-reflect']); }
