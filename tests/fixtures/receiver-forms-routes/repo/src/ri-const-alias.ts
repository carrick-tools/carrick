import make from 'fixture-server';
const app = make();
const a2 = app;
a2.get('/p/ri-const-alias', (_q, r) => r.send([]));
