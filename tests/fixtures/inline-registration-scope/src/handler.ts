// A route handler's callback receives a request, not an instance.
export function routes(router: any, auth: () => unknown) {
  router.get('/me', auth(), async (c: any) => {
    const user = c.get('user');
    return c.json(user);
  });
}
