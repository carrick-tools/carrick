import { FastifyPluginAsync } from "fastify";

const routes: FastifyPluginAsync = async (server) => {
  server.get("/daily", async (request, reply) => {
    return [];
  });
};

export default routes;
