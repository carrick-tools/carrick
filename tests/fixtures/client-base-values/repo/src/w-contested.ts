import mk, { Inst } from '@fixture/mk';
function send(api: Inst) { api.defaults.baseURL = 'https://x.test'; return api.get('/p/w-contested'); }
export function f() { return send(mk.create({ baseURL: 'https://m.test' })); }
