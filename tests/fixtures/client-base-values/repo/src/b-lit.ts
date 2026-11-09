import mk, { Inst } from '@fixture/mk';
const api = mk.create({ baseURL: 'https://m.test' });
export function f() { return api.get('/p/b-lit'); }
