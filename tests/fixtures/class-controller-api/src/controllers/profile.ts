import { Controller, Context } from '../framework';

class ProfileController extends Controller {
  get(ctx: Context): void {
    ctx.response.body = { id: 'a', displayName: 'first' };
  }

  patch(ctx: Context): void {
    // A JSON-schema `$id` URL. It is an argument to a validator, never a path
    // this service serves.
    ctx.request.validate('https://example.invalid/schemas/profile-update.json');
    ctx.response.body = ctx.request.body;
  }
}

export default new ProfileController();
