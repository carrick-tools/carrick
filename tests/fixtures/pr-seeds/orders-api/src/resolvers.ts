export interface OrderView {
  id: string;
  total: number;
  currency: string;
}

const orders = new Map<string, OrderView>();

export const resolvers = {
  Query: {
    order: (_parent: unknown, args: { id: string }): OrderView | null => orders.get(args.id) ?? null,
  },
};
