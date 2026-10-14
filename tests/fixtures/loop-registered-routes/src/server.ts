import { planRoutes } from './plan-routes.js';

export async function server(app: any) {
  await app.register(planRoutes, { prefix: '/api/v1' });
}
