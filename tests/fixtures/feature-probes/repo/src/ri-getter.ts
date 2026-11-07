import make from 'fixture-server';
const inst = make();
class Srv { get app() { return inst; } reg() { this.app.get('/p/ri-getter', (_q, r) => r.send([])); } }
new Srv().reg();
