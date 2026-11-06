import make from 'fixture-server';
import type { App } from 'fixture-server';
class Srv { constructor(private app: App) {} reg() { this.app.get('/p/ri-param-property', (_q, r) => r.send([])); } }
new Srv(make()).reg();
