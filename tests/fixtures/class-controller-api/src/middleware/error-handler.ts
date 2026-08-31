import type { Middleware } from '../framework';

// A plain middleware, not a controller. It is bound in front of `/token`, so a
// binding rule that took the FIRST identifier argument would attribute that
// route here and find no handler methods at all.
const errorHandler: Middleware = async (ctx, next) => {
  try {
    await next();
  } catch (err) {
    ctx.response.status = 500;
    ctx.response.body = { message: (err as Error).message };
  }
};

export default errorHandler;
