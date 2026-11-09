import mk, { Inst } from '@fixture/mk';
export const opts = { baseURL: 'https://m.test' };
const api = mk.create(opts);
export function f() { return api.get('/p/b-opts-exported'); }
