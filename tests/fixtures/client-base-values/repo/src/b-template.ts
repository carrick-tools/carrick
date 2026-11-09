import mk, { Inst } from '@fixture/mk';
const HOST = 'm.test';
const api = mk.create({ baseURL: `https://${HOST}` });
export function f() { return api.get('/p/b-template'); }
