import { Controller, Context } from '../framework';

class WidgetController extends Controller {
  get(ctx: Context): void {
    ctx.response.body = [{ id: '1', name: 'first' }];
  }

  post(ctx: Context): void {
    ctx.response.body = ctx.request.body;
    ctx.response.status = 201;
  }
}

export default new WidgetController();
