// A mounted plugin that mounts further plugins. Its own framework instance is
// the parameter `server` — the same name the root file uses, and the same name
// every leaf router uses, so the parent of these mounts cannot be identified
// by name alone.
import { registerCatalogRouter } from "./catalog-router.js";
import { registerInventoryRouter } from "./inventory-router.js";

export const registerApiRoutes = async (server: FastifyInstance) => {
  await server.register(registerCatalogRouter, { prefix: "/catalog" });
  await server.register(registerInventoryRouter, { prefix: "/inventory" });
};
