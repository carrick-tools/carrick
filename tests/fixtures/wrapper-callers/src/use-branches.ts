import {
  BothBranches,
  BranchClient,
  CallbackWrite,
  DefaultedKey,
  OrQuery,
  Reassigned,
  TwoLocals,
} from "./clients/branches";

const both = new BothBranches();
const reassigned = new Reassigned();
const orQuery = new OrQuery();
const callbackWrite = new CallbackWrite();
const branching = new BranchClient();
const twoLocals = new TwoLocals();
const defaultedKey = new DefaultedKey();

export async function act() {
  await both.post("/items.list");
  await reassigned.post("/items.list");
  await orQuery.post("/items.list", { limit: 3 });
  await both.post("https://other.test/x");
  await callbackWrite.post("/items.list", []);
  await branching.post("/items.list", { limit: 3 });
  await twoLocals.post("/items.list", { limit: 3 });
  await defaultedKey.post("/items.list");
  await defaultedKey.post("/items.list", {});
  await defaultedKey.post("/items.list", { mode: "next" });
}
