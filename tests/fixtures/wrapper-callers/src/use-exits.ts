import {
  AfterUse,
  ClosureRead,
  EarlyReturn,
  EarlyReturnGuarded,
  EmptyInit,
  EmptyNullish,
  InLoop,
  NullInit,
  QueryWritten,
  SameBlock,
  SelfExtend,
  ThrowBranch,
  UnwrittenAtUse,
} from "./clients/exits";

const early = new EarlyReturn();
const guarded = new EarlyReturnGuarded();
const empty = new EmptyInit();
const nullInit = new NullInit();
const emptyNullish = new EmptyNullish();
const after = new AfterUse();
const loop = new InLoop();
const closure = new ClosureRead();
const thrower = new ThrowBranch();
const unwritten = new UnwrittenAtUse();
const selfExtend = new SelfExtend();
const sameBlock = new SameBlock();
const queryWritten = new QueryWritten();

export async function act() {
  await early.post("/items.list");
  await guarded.send("/items.list", "GET");
  await empty.send("/items.list", "POST");
  await nullInit.send("/items.list", "POST");
  await emptyNullish.send("/items.list", "POST");
  await after.post("/items.list");
  await loop.post("/items.list", ["/a"]);
  await closure.post("/items.list");
  await thrower.post("/items.list");
  await unwritten.send("/items.list", "POST");
  await selfExtend.post("/items.list");
  await selfExtend.send("/items.list", "GET");
  await sameBlock.send("/items.list", "POST");
  await queryWritten.get("x");
}
