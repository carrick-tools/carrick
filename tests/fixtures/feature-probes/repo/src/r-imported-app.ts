import { app } from './lib/app';
app.get('/p/r-imported-app', (_q, r) => r.send([]));
