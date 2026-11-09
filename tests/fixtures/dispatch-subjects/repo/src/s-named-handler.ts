import { createServer } from "node:http";
import type { IncomingMessage, ServerResponse } from "node:http";
const handler = (req: IncomingMessage, res: ServerResponse) => {
  if (req.method === "GET" && req.url === "/s-named-handler") { res.end("a"); }
};
createServer(handler);
