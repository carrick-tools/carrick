import { Controller, Get, Post } from "./framework";

// The prefix is the `path` of an object argument; the other keys are not read.
@Controller({ path: "orders", version: "2" })
export class OrdersController {
  @Get()
  list(): string[] {
    return [];
  }

  @Post()
  create(): string {
    return "created";
  }
}

// The prefix is a template literal with no substitution, and so is a method path.
@Controller({ path: `reports`, scope: 1 })
export class ReportsController {
  @Get(`daily`)
  daily(): string {
    return "daily";
  }
}

// An object with no `path` key states the empty prefix.
@Controller({ version: "1" })
export class RootController {
  @Get("ping")
  ping(): string {
    return "pong";
  }
}
