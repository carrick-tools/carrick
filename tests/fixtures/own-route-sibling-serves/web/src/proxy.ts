import { createServer } from "http-server";

const app = createServer();

// Every request under /api is passed on to the service behind this one.
app.get("/api/*", (request, reply) => {
  reply.forward(request);
});
