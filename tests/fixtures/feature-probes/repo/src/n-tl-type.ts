import { http } from '@fixture/http';
function send(p: `/p/${string}`) { return http.post(p, null); }
export function f() { return send('/p/n-tl-type'); }
