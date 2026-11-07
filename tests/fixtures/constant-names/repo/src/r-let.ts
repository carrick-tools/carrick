import make from 'fixture-server';
let PATH = '/r/let';
const app = make();
app.get(PATH, (_q, r) => r.send([]));
