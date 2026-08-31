import { Controller, Context } from '../framework';

class SessionController extends Controller {
  get(ctx: Context): void {
    ctx.response.body = [{ id: 'a', createdAt: '2020-01-01' }];
  }

  delete(ctx: Context): void {
    ctx.response.status = 204;
  }
}

// Default-exported through a local binding rather than inline.
const session = new SessionController();
export default session;
