import make from 'fixture-server';
class Srv { get app() { return make(); } reg() { this.app.get('/p/s-getter-builds', (_q, r) => r.send([])); } }
new Srv().reg();
