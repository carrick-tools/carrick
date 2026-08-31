import { Controller, Context } from '../framework';

class WidgetItemController extends Controller {
  get(ctx: Context): void {
    ctx.response.body = { id: ctx.params.id, name: 'first' };
  }

  put(ctx: Context): void {
    ctx.response.body = { id: ctx.params.id, ...(ctx.request.body as object) };
  }

  delete(ctx: Context): void {
    ctx.response.status = 204;
  }
}

export default new WidgetItemController();
