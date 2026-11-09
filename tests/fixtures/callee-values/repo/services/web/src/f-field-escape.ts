import { send } from "./send";

declare function register(client: unknown): void;

export class Client {
  f: typeof send;
  g: typeof send;

  constructor() {
    register(this);
    this.f = send;
  }

  go() {
    return this.f("/p/f-field-escape");
  }

  setup() {
    this.g = send;
  }

  other() {
    return this.g("/p/f-field-escape-g");
  }
}
