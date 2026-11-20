import { Hub } from "hub-kit";

export class Orders {
  constructor(private hub: Hub) {}

  ship() {
    this.hub.fire("orders.shipped", {});
  }
}
