import { Local } from "hub-kit";

export class Orders {
  constructor(private hub: Local) {}

  ship() {
    this.hub.fire("orders.shipped", {});
  }
}
