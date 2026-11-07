import mk from '@fixture/mk';
const api = mk.create({ baseURL: 'https://m.test' });
const a2 = api;
export function f() { return a2.get('/p/m-const-alias'); }
