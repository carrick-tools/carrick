import { Controller, Get, Post } from "./framework";
import { ApiTags } from "./docs";
import { Trace } from "./observability";

export interface User {
  id: string;
  name: string;
}

// The prefix is on the class, the verb and the path are on each method. Two
// class decorators state a string; the routing one is the one imported from
// the same module as the verbs.
@ApiTags("people")
@Controller("api/users")
export class UsersController {
  private readonly users: User[] = [{ id: "1", name: "Ada" }];

  // No argument at all: this route IS the prefix.
  @Get()
  list(): User[] {
    return this.users;
  }

  @Get(":id")
  find(id: string): User | undefined {
    return this.users.find((user) => user.id === id);
  }

  @Post(":id/rename")
  rename(id: string, name: string): User {
    return { id, name };
  }

  // Not a route: TRACE is an HTTP method by the letter of the spec, and a
  // decorator named after it is far likelier to be observability.
  @Trace()
  audit(): void {
    return;
  }

  // Not a route: no decorator names an HTTP method.
  private nextId(): string {
    return String(this.users.length + 1);
  }
}
