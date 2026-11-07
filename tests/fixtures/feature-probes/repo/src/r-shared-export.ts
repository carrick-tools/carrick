import { shared } from 'fixture-server';
shared.get('/p/r-shared', (_q, r) => r.send([]));
