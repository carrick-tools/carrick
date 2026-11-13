import {
  BranchToHelper,
  ExitBeforeSend,
  literalAfterExit,
  PrefixExit,
} from "./clients/path-guards";

const exit = new ExitBeforeSend();
const branch = new BranchToHelper();
const prefix = new PrefixExit();

export async function act(verb: string) {
  await exit.send("/items.list", "GET");
  await exit.send("/items.list", "POST");
  await exit.send("/items.list", verb);
  await branch.sync("PUSH");
  await branch.sync("PULL");
  await prefix.send("/items.list", { mode: "dry-run" });
  await prefix.send("/items.list", { mode: "live" });
  await literalAfterExit("GET");
  await literalAfterExit("PUT");
}
