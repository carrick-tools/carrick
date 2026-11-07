import make from 'fixture-server';
import type { App } from 'fixture-server';
const app: App | undefined = make();
app?.get('/p/ri-optional', (_q, r) => r.send([]));
