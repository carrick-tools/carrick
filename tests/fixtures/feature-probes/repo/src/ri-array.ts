import make from 'fixture-server';
const apps = [make()];
apps[0].get('/p/ri-array', (_q, r) => r.send([]));
