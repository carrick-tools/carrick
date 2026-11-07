import make from 'fixture-server';
enum R { A = '/r/enum' }
const app = make();
app.get(R.A, (_q, r) => r.send([]));
