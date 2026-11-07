import make from 'fixture-server';
const deps = { app: make() };
const { app: web } = deps;
web.get('/p/s-destructured', (_q, r) => r.send([]));
