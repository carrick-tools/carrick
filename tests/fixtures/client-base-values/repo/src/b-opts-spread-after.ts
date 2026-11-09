import mk, { Inst } from '@fixture/mk';
const defaults = { baseURL: 'https://other.test' };
const api = mk.create({ baseURL: 'https://m.test', ...defaults });
export function f() { return api.get('/p/b-opts-spread-after'); }
