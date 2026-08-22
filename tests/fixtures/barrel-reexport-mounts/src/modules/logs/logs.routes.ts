import { FastifyPluginAsync } from "fastify";

// Named differently from its siblings on purpose: the local symbol name must
// not be what decides which module a route belongs to.
const logsRoutes: FastifyPluginAsync = async (server) => {
  server.post("/entries", async (request, reply) => {
    return { ok: true };
  });
};

export default logsRoutes;
