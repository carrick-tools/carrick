import mk from '@fixture/mk';
const deps = { api: mk.create({ baseURL: 'https://m.test' }) };
const { api } = deps;
export function f() { return api.get('/p/m-destructured'); }
