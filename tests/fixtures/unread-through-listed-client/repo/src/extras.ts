import { client } from "./invoke";

export async function extra() {
  return client.extras.run("nightly");
}
