import make from 'fixture-server';
const app = make();
app.get('/p/r-chained-a', (_q, r) => r.send([])).post('/p/r-chained-b', (_q, r) => r.send({}));
