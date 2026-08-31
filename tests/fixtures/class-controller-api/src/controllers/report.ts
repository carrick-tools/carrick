import { Controller, Context, accept, method } from '../framework';

class ReportController extends Controller {
  // Not verb-named, so the method comes from the decorator literal.
  @method('GET')
  @accept('text/csv')
  exportCsv(ctx: Context): void {
    ctx.response.type = 'text/csv';
    ctx.response.body = this.buildRows().join('\n');
  }

  // Neither verb-named nor method-decorated: a helper, not a route. It must
  // emit nothing — and `@accept('text/csv')` above must never become a path.
  buildRows(): string[] {
    return ['id,name', '1,first'];
  }
}

export default new ReportController();
