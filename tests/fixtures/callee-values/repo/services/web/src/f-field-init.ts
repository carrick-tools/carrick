import { send } from "./send";

export class Client {
  f = send;

  go() {
    return this.f("/p/f-field-init");
  }
}
