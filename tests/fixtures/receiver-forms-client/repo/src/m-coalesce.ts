import mk, { Inst } from '@fixture/mk';
const def = mk.create({ baseURL: 'https://m.test' });
export function f(opts: { api?: Inst }) { return (opts.api ?? def).get('/p/m-coalesce'); }
