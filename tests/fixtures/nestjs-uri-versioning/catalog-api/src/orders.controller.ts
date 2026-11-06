import { Controller, Get, Post } from "./framework";

// The controller states its own version.
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
