import type { App } from 'fixture-plugin-server';
import { orderRoutes } from './orders';

export async function api(app: App) {
  app.register(async (scope: App) => {
    await scope.register(orderRoutes);
  });
}
