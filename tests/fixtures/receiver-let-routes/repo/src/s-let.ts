import make, { App } from 'fixture-server';
let app: App = make();
export function set(next: App) { app = next; }
app.get('/p/s-let', (_q, r) => r.send([]));
