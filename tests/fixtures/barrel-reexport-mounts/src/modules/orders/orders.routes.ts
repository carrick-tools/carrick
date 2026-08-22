import { FastifyPluginAsync } from "fastify";

const routes: FastifyPluginAsync = async (server) => {
  server.get("/pending", async (request, reply) => {
    return [];
  });
};

export default routes;
