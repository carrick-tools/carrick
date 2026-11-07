import make from 'fixture-server';
const app = make();
const { get } = app;
get.call(app, '/p/ri-destructure', (_q, r) => r.send([]));
