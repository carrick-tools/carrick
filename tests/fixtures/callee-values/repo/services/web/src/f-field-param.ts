import { send } from "./send";

export class Client {
  f = send;

  set(next: typeof send) {
    this.f = next;
  }

  go() {
    return this.f("/p/f-field-param");
  }
}
