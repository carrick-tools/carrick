// Root: the whole API is registered one level down, under a version prefix.
// Nothing here names the routers that eventually own the routes.
import { registerApiRoutes } from "./api/index.js";

export const buildServer = async (server: FastifyInstance) => {
  await server.register(registerApiRoutes, { prefix: "/api/v1" });
};
