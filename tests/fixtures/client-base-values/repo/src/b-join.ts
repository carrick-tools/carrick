import mk, { Inst } from '@fixture/mk';
const api = mk.create({ baseURL: ['https://m.test', 'v1'].join('/') });
export function f() { return api.get('/p/b-join'); }
