import { Controller, Get, VERSION_NEUTRAL, Version } from "./framework";

// The string form: no version of its own, so the service default applies.
@Controller("widgets")
export class WidgetsController {
  @Get()
  list(): string[] {
    return [];
  }

  @Get(":id")
  one(): string {
    return "widget";
  }

  @Version(VERSION_NEUTRAL)
  @Get("ping")
  ping(): string {
    return "pong";
  }
}
