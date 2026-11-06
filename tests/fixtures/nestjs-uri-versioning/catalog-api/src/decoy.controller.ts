import { Get } from "./framework";
import { Docs } from "./docs";

// The class decorator comes from another module than the verbs: it states no
// prefix, so the class states no route, whatever its `version` says.
@Docs({ path: "x", version: "9" })
export class DecoyController {
  @Get("decoy")
  decoy(): string {
    return "decoy";
  }
}
