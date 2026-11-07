import make, { App } from 'fixture-server';
const def = make();
export function reg(opts: { app?: App }) { (opts.app ?? def).get('/p/s-coalesce', (_q, r) => r.send([])); }
reg({});
