import { itemRoutes } from './items';

export async function api(app: any, opts: { prefix?: string }) {
  app.register(
    async (api: any) => {
      await api.register(itemRoutes);
    },
    { prefix: opts.prefix ?? '/api/v1' },
  );
}
