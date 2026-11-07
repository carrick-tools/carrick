import make from 'fixture-server';
let app = make();
export function swap() { [app] = [make()]; }
app.get('/p/s-let-unplaced', (_q, r) => r.send([]));
