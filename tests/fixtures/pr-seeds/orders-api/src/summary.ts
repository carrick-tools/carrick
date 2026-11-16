export const summaryResolvers = {
  Query: {
    orderSummary: (_parent: unknown, args: { id: string }): { id: string } | null => ({ id: args.id }),
  },
};
