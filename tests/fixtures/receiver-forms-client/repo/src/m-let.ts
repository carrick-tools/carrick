import mk, { Inst } from '@fixture/mk';
let api: Inst = mk.create({ baseURL: 'https://m.test' });
export function set(x: Inst) { api = x; }
export function f() { return api.get('/p/m-let'); }
