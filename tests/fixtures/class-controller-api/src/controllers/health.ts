import { Controller, Context } from '../framework';

// The other default-export shape: the class itself, exported inline.
export default class HealthController extends Controller {
  get(ctx: Context): void {
    ctx.response.body = { status: 'ok' };
  }
}
