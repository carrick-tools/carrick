import { itemRoutes } from './items';

export async function api(app: any, { prefix = '/api/v4' }: { prefix?: string } = {}) {
  app.register(
    async (api: any) => {
      await api.register(itemRoutes);
    },
    { prefix },
  );
}
