import make from 'fixture-server';
const app = make();
app.get('/p/r-literal', (_q, r) => r.send([]));
