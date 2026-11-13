import { createServer, Request, Reply } from "http-server";

export interface OpenBody {
  jobId: string;
}

export interface Opened {
  opened: boolean;
}

const app = createServer();

// Everything under /v1 not answered by a concrete route is passed on.
app.all("/v1/*", (request: Request<{ scope: string }>, reply: Reply<unknown>) => {
  reply.forward(request);
});

app.post("/v1/jobs/open", (request: Request<OpenBody>, reply: Reply<Opened>) => {
  const result: Opened = { opened: request.body.jobId.length > 0 };
  reply.json(result);
});
