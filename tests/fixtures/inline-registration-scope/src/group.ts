import { orderRoutes } from './orders';

export function routes(app: any) {
  app.group('/v2', (g: any) => g.register(orderRoutes));
}
