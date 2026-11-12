import { db } from "../db";

type Context = { user: string | null };

export const resolvers = {
  Query: {
    order: async (_parent: unknown, args: { id: string }, context: Context) => {
      if (!context.user) throw new Error("unauthorised");
      return db.orders.find(args.id);
    },
  },
};
