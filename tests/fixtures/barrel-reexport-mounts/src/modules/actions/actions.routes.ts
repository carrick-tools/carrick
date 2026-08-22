import { FastifyPluginAsync } from "fastify";

const routes: FastifyPluginAsync = async (server) => {
  server.post("/actions", async (request, reply) => {
    return { ok: true };
  });
};

export default routes;
