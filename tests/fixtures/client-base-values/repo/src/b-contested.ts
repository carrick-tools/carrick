import mk, { Inst } from '@fixture/mk';
const api = mk.create({ baseURL: 'https://m.test' });
export function reset(u: string) { api.defaults.baseURL = u; }
export function f() { return api.get('/p/b-contested'); }
