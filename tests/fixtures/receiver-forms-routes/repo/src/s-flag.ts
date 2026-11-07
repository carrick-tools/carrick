import make from 'fixture-server';
const a = make();
const b = make();
const flag = process.argv.length > 2;
(flag ? a : b).get('/p/s-flag', (_q, r) => r.send([]));
