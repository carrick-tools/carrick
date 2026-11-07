import make from 'fixture-server';
const app = make();
app?.get('/p/o-app', (_q, r) => r.send([]));
