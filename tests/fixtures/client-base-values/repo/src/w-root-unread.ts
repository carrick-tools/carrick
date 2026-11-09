import mk, { Inst } from '@fixture/mk';
declare const pool: Inst[];
function send(api: Inst) { return api.get('/p/w-root-unread'); }
export function f() { return send(mk.create({ baseURL: 'https://m.test' })); }
export function g() { return send(pool[0]); }
