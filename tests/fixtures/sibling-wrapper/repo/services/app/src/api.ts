import { getItem, getStatus, getVersion } from "@fixture/client";

/** A client wrapper: each method calls the sibling package's function. */
export class ItemsApi {
  get(id: string) {
    return getItem(id);
  }

  status() {
    return getStatus();
  }

  both() {
    getVersion();
    return getItem("1");
  }
}
