import mk, { Inst } from '@fixture/mk';
const defaults = { baseURL: 'https://m.test' };
const api = mk.create({ ...defaults });
export function f() { return api.get('/p/b-opts-spread-only'); }
