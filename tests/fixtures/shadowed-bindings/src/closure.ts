export async function closureShadow(ids: string[]) {
  const ORDERS = "/api/orders";
  return Promise.all(
    ids.map(async (id) => {
      if (id) {
        const ORDERS = "/api/archived-orders";
        return fetch(ORDERS, { method: "PATCH" });
      }
      return fetch(ORDERS, { method: "GET" });
    }),
  );
}
