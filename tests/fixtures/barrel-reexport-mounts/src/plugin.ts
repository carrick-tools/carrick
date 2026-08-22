import { FastifyInstance } from "fastify";
import {
  actionsRoutes,
  sessionsRoutes,
  filesRoutes,
  logsRoutes,
} from "./routes.js";

export async function registerRoutes(fastify: FastifyInstance) {
  await fastify.register(actionsRoutes, { prefix: "/v1" });
  await fastify.register(sessionsRoutes, { prefix: "/v1" });
  await fastify.register(filesRoutes, { prefix: "/v1" });
  await fastify.register(logsRoutes, { prefix: "/v1/logs" });
}
