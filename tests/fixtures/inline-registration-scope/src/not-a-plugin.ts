import { onError } from './errors';

export async function api(app: any, opts: { prefix?: string }) {
  app.register(
    async (api: any) => {
      api.setErrorHandler(onError);
    },
    { prefix: opts.prefix ?? '/api/v1' },
  );
}
