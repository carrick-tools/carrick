import mk, { Inst } from '@fixture/mk';
function send(api: Inst) { return api.get('/p/m-param'); }
export function f() { return send(mk.create({ baseURL: 'https://m.test' })); }
