import { configure } from "fixture-wire";

export function setUp() {
  configure({ onIdle: () => {} });
}
