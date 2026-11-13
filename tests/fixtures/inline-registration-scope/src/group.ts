import type { App } from 'fixture-plugin-server';
import { orderRoutes } from './orders';

export function routes(app: App) {
  app.group('/v2', (g: App) => g.register(orderRoutes));
}
