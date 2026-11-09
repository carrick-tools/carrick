import mk from '@fixture/mk';
const api = mk.create({ baseURL: 'https://m.test' });
const deps = { client: api };
const c = deps.client;
export function f() { return c.get('/p/m-entry-alias'); }
