import axios from "axios";
import { Get } from "./framework";
import { ApiTags } from "./docs";

// 1. A bare path literal with no base at all. Whether this registers a route
// or requests one is not a structural fact, and no rule here may guess
// (ruling, 2026-09-05).
export async function bare(): Promise<unknown> {
  const response = await axios.get("/api/users/1");
  return response.data;
}

// 2. A base declared as a string literal, backed by nothing but its own
// initialiser. Not the environment, so the env-base rule is silent on it
// (carrick#627/#641 own this shape).
const LITERAL_BASE = "https://api.example.com";

export async function literalBase(): Promise<unknown> {
  const response = await axios.get(`${LITERAL_BASE}/api/things`);
  return response.data;
}

// 3. A base the file cannot see behind: an injected option.
export class Injected {
  constructor(private readonly opts: { lookupUrl: string }) {}

  async lookup(): Promise<unknown> {
    const response = await axios.get(`${this.opts.lookupUrl}/api/lookup`);
    return response.data;
  }
}

// 4. A class whose methods carry verb decorators but whose declaration states
// no prefix. Reading a route here would mean treating every undecorated class
// as a routing claim.
export class Unprefixed {
  @Get("orphan")
  orphan(): string {
    return "orphan";
  }
}

// 5. A class whose only string-stating decorator is a documentation tag from
// another module. The routing decorator here would be an argument-less one,
// which states no prefix, and reading the tag's string instead would put a
// path nothing serves into the index.
@ApiTags("people")
export class Tagged {
  @Get(":id")
  find(): string {
    return "tagged";
  }
}
