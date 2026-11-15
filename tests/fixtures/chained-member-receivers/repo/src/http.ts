import { Courier } from "fixture-courier";

// (h1) Held: the member call on a held client, the op on a held value.
export async function heldArea() {
  const courier = Courier.connect({ baseURL: "https://c.test" });
  const area = courier.area("/v1");
  return area.get("/users");
}

// (h2) In place: the member call on a call's value, the op on its value.
export async function chainedArea() {
  return Courier.connect({ baseURL: "https://c.test" }).area("/v1").post("/users", {});
}

// (h3) On a held client: the member call, then the op on its value.
const shared = Courier.connect({ baseURL: "https://c.test" });
export async function sharedArea() {
  return shared.area("/v2").get("/orders");
}
