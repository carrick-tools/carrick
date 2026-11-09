import { send } from "./send";

export class Client {
  f: typeof send;
  g: typeof send;

  constructor() {
    this.g = send;
    this.f = this.g;
  }

  go() {
    return this.f("/p/f-field-ctor-copy");
  }
}
