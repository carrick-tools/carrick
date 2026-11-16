// Synthetic GraphQL resolvers. Every name is invented; held-place-graphql.test.ts
// wraps the returned values below.

type OrderShape = { id: string; total: number };

declare function loadOrder(id: string): Promise<OrderShape>;
declare const payload: any;

export const orderResolvers = {
  async order(_p: unknown, args: { id: string }) {
    return loadOrder(args.id);
  },
  orders() {
    return { edges: [{ id: "1", total: 2 }], totalCount: 1 };
  },
  archived() {
    return null;
  },
  paid() {
    return true;
  },
  raw() {
    return payload;
  },
};
