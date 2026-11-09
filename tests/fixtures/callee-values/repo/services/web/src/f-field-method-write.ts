import { other, send } from "./send";

export class Client {
  f: typeof send;

  constructor() {
    this.f = send;
  }

  swap() {
    this.f = other as typeof send;
  }

  go() {
    return this.f("/p/f-field-method-write");
  }
}
