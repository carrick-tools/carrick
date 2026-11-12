import type { App } from 'fixture-plugin-server';

// A route handler's callback receives a request, not an instance.
export function routes(router: App,auth: () => unknown) {
  router.get('/me', auth(), async (c: any) => {
    const user = c.get('user');
    return c.json(user);
  });
}
