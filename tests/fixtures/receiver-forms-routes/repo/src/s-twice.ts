import make, { App } from 'fixture-server';
class Srv { private app: App; constructor() { this.app = make(); } reset() { this.app = make({ fresh: true }); } reg() { this.app.get('/p/s-twice', (_q, r) => r.send([])); } }
new Srv().reg();
