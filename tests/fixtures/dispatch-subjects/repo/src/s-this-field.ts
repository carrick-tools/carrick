import { createServer } from "node:http";
import type { IncomingMessage, ServerResponse } from "node:http";
class H {
  constructor(private req: IncomingMessage, private res: ServerResponse) {}
  run() {
    if (this.req.method === "GET" && this.req.url === "/s-this-field") { this.res.end("a"); }
  }
}
createServer((req, res) => new H(req, res).run());
