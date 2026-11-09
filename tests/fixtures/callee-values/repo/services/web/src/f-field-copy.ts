import { send } from "./send";

export class Client {
  g = send;
  f = this.g;

  go() {
    return this.f("/p/f-field-copy");
  }
}
