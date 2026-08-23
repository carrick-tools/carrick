// A second leaf router declaring the same two relative paths as the first.
export const registerInventoryRouter = async (server: FastifyInstance) => {
  server.post("/", async () => ({ created: true }));
  server.get("/:id", async () => ({ id: "1" }));
};
