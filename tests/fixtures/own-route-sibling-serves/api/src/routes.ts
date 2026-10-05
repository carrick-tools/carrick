import { createServer } from "http-server";

export interface Order {
  id: string;
  total: number;
}

const app = createServer();

app.get("/api/orders/:orderId", (request, reply) => {
  const order: Order = { id: request.params.orderId, total: 12 };
  reply.json(order);
});
