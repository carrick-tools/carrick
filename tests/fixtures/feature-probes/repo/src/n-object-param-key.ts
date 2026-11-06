import { http } from '@fixture/http';
function send({ url }: { url: string }) { return http.post(url, null); }
export function f() { return send({ url: '/p/n-object-param-key' }); }
