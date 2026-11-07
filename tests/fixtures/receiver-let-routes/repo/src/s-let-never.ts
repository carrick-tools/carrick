import make from 'fixture-server';
let app = make();
app.get('/p/s-let-never', (_q, r) => r.send([]));
