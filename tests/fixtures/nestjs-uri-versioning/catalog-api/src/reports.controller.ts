import { Controller, Get, Version } from "./framework";

// A template-literal path, and a handler that overrides the default.
@Controller({ path: `reports`, scope: 1 })
export class ReportsController {
  @Version("3")
  @Get("daily")
  daily(): string {
    return "daily";
  }

  @Get("weekly")
  weekly(): string {
    return "weekly";
  }
}
