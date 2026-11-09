import { createServer } from "node:http";
import type { IncomingMessage, ServerResponse } from "node:http";
const ROUTES = { health: "/s-route-table" } as const;
createServer((req, res) => {
  if (req.method === "GET" && req.url === ROUTES.health) { res.end("a"); }
});
