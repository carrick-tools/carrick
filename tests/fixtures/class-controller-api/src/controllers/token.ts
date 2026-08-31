import { Controller, Context } from '../framework';

class TokenController extends Controller {
  post(ctx: Context): void {
    const grant = ctx.request.body as { grantType: string };
    ctx.response.body = { accessToken: 'opaque', grantType: grant.grantType };
  }
}

export default new TokenController();
