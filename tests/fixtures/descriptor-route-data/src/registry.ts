// A route table: every record names the handler that serves it.
const listOrders = () => [{ id: 'o1' }];

export const routeTable = [
  { method: 'GET', path: '/orders', handler: listOrders },
  { method: 'POST', path: '/orders', handler: async () => ({ created: true }) },
  { method: 'GET', path: '/orders/:id', handler: 'orders.get' },
];
