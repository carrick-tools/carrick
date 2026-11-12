import { http } from "./http";

export const resolvers = {
  Mutation: {
    async refund(_parent: unknown, args: { orderId: string }) {
      return http.post("/events", { type: "refund", orderId: args.orderId });
    },
  },
};
