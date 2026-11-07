import make from 'fixture-server';
class Srv {
  app = make();
  router = make();
  reg() {
    this.router.get('/p/s-mount', (_q, r) => r.send([]));
    this.app.route('/api', this.router);
  }
}
new Srv().reg();
