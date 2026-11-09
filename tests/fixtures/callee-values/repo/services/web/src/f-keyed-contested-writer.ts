import { other, send } from "./send";
import { d } from "./f-keyed-contested";

export function swap() {
  d.a = other as typeof send;
}
