import { shared } from 'fixture-server';
shared.get('/p/s-shared', (_q, r) => r.send([]));
