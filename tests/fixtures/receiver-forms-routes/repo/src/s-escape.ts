import make, { App } from 'fixture-server';
const seen: unknown[] = [];
class Srv { app: App = make(); constructor() { seen.push(this); } reg() { this.app.get('/p/s-escape', (_q, r) => r.send([])); } }
new Srv().reg();
