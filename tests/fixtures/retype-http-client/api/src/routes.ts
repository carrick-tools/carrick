import { createServer } from "http-server";

export interface CheckoutResult {
  y: number;
}

const app = createServer();

app.post("/checkout", (request, reply) => {
  const result: CheckoutResult = { y: 1 };
  reply.json(result);
});
