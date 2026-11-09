import mk, { Inst } from '@fixture/mk';
const defaults = { timeout: 1 };
const api = mk.create({ ...defaults, baseURL: 'https://m.test' });
export function f() { return api.get('/p/b-opts-spread'); }
