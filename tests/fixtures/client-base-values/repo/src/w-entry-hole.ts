import mk, { Inst } from '@fixture/mk';
function send(api: Inst) { return api.get('/p/w-entry-hole'); }
export function f() { return send(mk.create({ baseURL: 'https://m.test' })); }
export function entry(api: Inst) { return send(api); }
