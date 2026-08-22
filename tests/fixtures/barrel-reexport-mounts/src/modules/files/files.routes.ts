import { FastifyPluginAsync } from "fastify";

const routes: FastifyPluginAsync = async (server) => {
  server.post("/files", async (request, reply) => {
    return { ok: true };
  });
};

export default routes;
