import router from "fixture-router";
import { regD } from "./registries";

declare const process: { argv: string[] };

// D: the destructured key in a for-of body at module scope.
const app = router();
for (const name of process.argv) {
  const { default: init } = await regD[name as keyof typeof regD]();
  init(app);
}
