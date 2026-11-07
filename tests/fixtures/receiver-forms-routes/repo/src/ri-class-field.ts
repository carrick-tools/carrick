import make from 'fixture-server';
class Srv { app = make(); reg() { this.app.get('/p/ri-class-field', (_q, r) => r.send([])); } }
new Srv().reg();
