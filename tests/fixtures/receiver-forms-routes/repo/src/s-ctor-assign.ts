import make, { App } from 'fixture-server';
class Srv { private app: App; constructor() { this.app = make(); } reg() { this.app.get('/p/s-ctor-assign', (_q, r) => r.send([])); } }
new Srv().reg();
