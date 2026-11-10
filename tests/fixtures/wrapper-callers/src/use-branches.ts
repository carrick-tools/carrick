import {
  BothBranches,
  BranchClient,
  CallbackWrite,
  OrQuery,
  Reassigned,
} from "./clients/branches";

const both = new BothBranches();
const reassigned = new Reassigned();
const orQuery = new OrQuery();
const callbackWrite = new CallbackWrite();
const branching = new BranchClient();

export async function act() {
  await both.post("/items.list");
  await reassigned.post("/items.list");
  await orQuery.post("/items.list", { limit: 3 });
  await both.post("https://other.test/x");
  await callbackWrite.post("/items.list", []);
  await branching.post("/items.list", { limit: 3 });
}
