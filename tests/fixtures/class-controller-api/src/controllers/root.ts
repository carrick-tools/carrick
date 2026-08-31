import { Controller, Context } from '../framework';

class RootController extends Controller {
  get(ctx: Context): void {
    ctx.response.body = { service: 'class-controller-api' };
  }
}

export default new RootController();
