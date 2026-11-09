import mk, { Inst } from '@fixture/mk';
const api = mk.create({ baseURL: process.env.API_URL ?? 'https://m.test' });
export function f() { return api.get('/p/b-env-fallback'); }
