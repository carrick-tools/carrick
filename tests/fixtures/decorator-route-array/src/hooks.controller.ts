import { Controller, Get, Post } from "./framework";

const MIRROR = "/internal/mirror";

@Controller("api")
export class HooksController {
  // An array of two literal paths: two routes, one handler.
  @Post(["/hooks/orders", "/internal/orders-hook"])
  receive(): string {
    return "ok";
  }

  // One literal and one name: one route, and the name is saved as unread.
  @Post(["/hooks/refunds", MIRROR])
  refund(): string {
    return "ok";
  }

  // A single string, as always.
  @Get("status")
  status(): string {
    return "up";
  }
}
