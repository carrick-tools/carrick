import { send, sendToo } from "./send";

export class Client {
  f = send;

  swap() {
    this.f = sendToo;
  }

  go() {
    return this.f("/p/f-field-method-agree");
  }
}
