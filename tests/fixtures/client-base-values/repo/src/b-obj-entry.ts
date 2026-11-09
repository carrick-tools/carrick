import mk, { Inst } from '@fixture/mk';
const CONFIG = { base: 'https://m.test' };
const api = mk.create({ baseURL: CONFIG.base });
export function f() { return api.get('/p/b-obj-entry'); }
