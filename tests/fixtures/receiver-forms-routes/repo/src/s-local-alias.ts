import make from 'fixture-server';
const app = make();
function f() { const r = app; r.get('/p/s-local-alias', (_q, res) => res.send([])); }
f();
