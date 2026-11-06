import { Controller, Get, VERSION_NEUTRAL } from "./framework";

// Neutral at the controller: served without a version segment.
@Controller({ path: "status/health", version: VERSION_NEUTRAL })
export class HealthController {
  @Get()
  check(): string {
    return "ok";
  }
}
