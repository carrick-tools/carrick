import make from 'fixture-server';
const app = make();
function getApp() { return app; }
getApp().get('/p/ri-call-result', (_q, r) => r.send([]));
