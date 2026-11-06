import { Controller, Get } from "./framework";
import { Docs } from "./docs";

const dynamic = "computed";

// The object-form decorator comes from another module than the verbs: no prefix.
@Docs({ path: "elsewhere" })
export class NotRouted {
  @Get("decoy-docs")
  one(): string {
    return "one";
  }
}

// A `path` that is not a literal states nothing, so the class is not read.
@Controller({ path: dynamic })
export class ComputedPrefix {
  @Get("decoy-computed")
  two(): string {
    return "two";
  }
}

// An array of paths is out of this reading.
@Controller({ path: ["a", "b"] })
export class ManyPrefixes {
  @Get("decoy-array")
  three(): string {
    return "three";
  }
}
