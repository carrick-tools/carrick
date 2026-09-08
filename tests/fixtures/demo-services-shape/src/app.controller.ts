import { Controller, Get, Sse } from "./framework";

export interface Health {
  status: string;
}

// The routing decorator states no path at all, so these routes hang off the
// root. Declining to read that left them to the model, which named one after
// its handler (carrick#804).
@Controller()
export class AppController {
  // No prefix and no path: the route IS the root.
  @Get()
  getHello(): string {
    return "hello";
  }

  @Get("health")
  getHealth(): Health {
    return { status: "ok" };
  }
}

// A server-sent-events route. `@Sse` is not one of the seven verbs, and the
// route it registers is a GET whose response is a stream.
@Controller("realtime")
export class RealtimeController {
  @Sse("stream")
  stream(): Health {
    return { status: "streaming" };
  }
}
