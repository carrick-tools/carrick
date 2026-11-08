import { listen, type WireRequest } from "fixture-wire";
import { Reply } from "./reply";

type Shape = Record<string, unknown>;

type Handler<TParams extends Shape = Shape, TBody extends Shape = Shape> = (ctx: {
  params: TParams;
  body: TBody;
  reply: Reply;
}) => Promise<unknown>;

interface RouteDef<TParams extends Shape = Shape, TBody extends Shape = Shape> {
  keepAlive?: boolean;
  handler: Handler<TParams, TBody>;
}

type Verb = "GET" | "POST";

type RelayOptions = { expose?: boolean };

export class RelayServer {
  private table: Partial<Record<string, Partial<Record<Verb, RouteDef>>>> = {};
  private fns: Record<string, Handler> = {};
  private count = 0;
  private last = "";

  readonly server = listen(async (req: WireRequest) => {
    const entry = this.table[req.path]?.[req.verb as Verb];
    if (entry) {
      await entry.handler({ params: {}, body: {}, reply: new Reply(req) });
    }
  });

  constructor(opts: RelayOptions) {
    if (opts.expose) {
      this.add("/gauges", "GET", { handler: async ({ reply }) => reply.text("ok") });
    }
  }

  add<TParams extends Shape = Shape, TBody extends Shape = Shape>(
    path: `/${string}`,
    verb: Verb,
    def: RouteDef<TParams, TBody>,
  ) {
    this.table[path] = { ...this.table[path], [verb]: def };
    return this;
  }

  addFn(path: string, fn: Handler) {
    this.fns[path] = fn;
  }

  wire() {
    this.add("/pulse", "GET", { handler: async ({ reply }) => reply.text("up") });
    this.addFn("/beat", async ({ reply }) => reply.text("b"));
  }

  runNow(fn: () => void) {
    fn();
  }

  tally() {
    this.runNow(() => {
      this.count += 1;
    });
  }

  record(path: string) {
    this.last = path;
  }

  note() {
    const later = async () => this.count;
    this.record("/note");
    return later;
  }
}
