import { itemDetailRoutes } from './item-detail';

export async function api(app: any, opts: { prefix?: string }) {
  app.register(
    async (api: any) => {
      await api.register(itemDetailRoutes);
    },
    { prefix: opts.prefix ?? '/api/v1' },
  );
}
