import { http } from '@fixture/http';
function make() { return (p: string) => http.post(p, null); }
export function f() { return make()('/p/t-closure'); }
