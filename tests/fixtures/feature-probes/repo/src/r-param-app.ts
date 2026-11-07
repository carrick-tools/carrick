import make from 'fixture-server';
import type { App } from 'fixture-server';
function register(app: App) { app.get('/p/r-param-app', (_q, r) => r.send([])); }
register(make());
