import { orderRoutes } from './orders';

export async function api(app: any) {
  app.register(async (scope: any) => {
    await scope.register(orderRoutes);
  });
}
