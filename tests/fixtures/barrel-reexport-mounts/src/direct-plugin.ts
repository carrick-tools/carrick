import { FastifyInstance } from "fastify";
// No barrel: both modules are imported directly, and both export a default
// named `routes`.
import ordersRoutes from "./modules/orders/orders.routes.js";
import reportsRoutes from "./modules/reports/reports.routes.js";

export async function registerDirectRoutes(fastify: FastifyInstance) {
  await fastify.register(ordersRoutes, { prefix: "/v1/orders" });
  await fastify.register(reportsRoutes, { prefix: "/v1/reports" });
}
