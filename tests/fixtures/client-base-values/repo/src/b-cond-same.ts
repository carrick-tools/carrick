import mk, { Inst } from '@fixture/mk';
const api = mk.create({ baseURL: process.env.X ? 'https://m.test' : 'https://m.test' });
export function f() { return api.get('/p/b-cond-same'); }
