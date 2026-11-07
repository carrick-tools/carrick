import make from 'fixture-server';
const app = make();
for (const period of ['daily', 'weekly'] as const) { app.get(`/p/r-loop/${period}`, (_q, r) => r.send([])); }
