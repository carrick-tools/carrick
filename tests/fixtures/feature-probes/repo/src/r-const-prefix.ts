import make from 'fixture-server';
const PREFIX = '/p/r-const-prefix';
const app = make();
app.get(`${PREFIX}/items`, (_q, r) => r.send([]));
