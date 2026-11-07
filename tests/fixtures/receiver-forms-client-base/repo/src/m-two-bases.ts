import mk, { Inst } from '@fixture/mk';
function send(api: Inst) { return api.get('/p/m-two-bases'); }
export function f() { return send(mk.create({ baseURL: 'https://m.test' })); }
export function g() { return send(mk.create({ baseURL: 'https://other.test' })); }
