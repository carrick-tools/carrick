// Leaf router. Both routes are declared relative to the mount prefix, so
// their paths are only distinguishable once the whole chain is applied.
export const registerCatalogRouter = async (server: FastifyInstance) => {
  server.post("/", async () => ({ created: true }));
  server.get("/:id", async () => ({ id: "1" }));
};
