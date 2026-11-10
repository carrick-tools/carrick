// Parameters and the calls that fill them (carrick#3504 B10-1). Each probe is
// a call of a `p_*` function whose argument is the name a reader asks about;
// the functions that make the calls sit above them and are called at the
// bottom, so the test finds a probe by its callee name. Every parameter is
// typed `string`: a literal type would be read from the type first.
export {};

declare const argv: string[];
declare function now(): number;

declare function p_one(name: string): void;
declare function p_two(name: string): void;
declare function p_keyed(name: string): void;
declare function p_keyed_default(name: string): void;
declare function p_member(name: string): void;
declare function p_defaulted(name: string): void;
declare function p_pair(name: string): void;
declare function p_uncalled(name: string): void;
declare function p_outer(name: string): void;
declare function p_callback(name: string): void;
declare function p_loop(name: string): void;
declare function p_ping(name: string): void;
declare function p_joined(name: string): void;
declare function p_lookup(name: string): void;
declare function p_grow(name: string): void;
declare function p_written(name: string): void;
declare function p_send(name: string): void;
declare function p_runtime(name: string): void;

const TABLE: Record<string, string> = { a: "t.a" };
const names: string[] = ["spread.a", "spread.b"];

function one(name: string): void {
  p_one(name);
}

function two(name: string): void {
  p_two(name);
}

function keyed({ topic }: { topic: string }): void {
  p_keyed(topic);
}

function keyedDefault({ topic = "jobs.default" }: { topic?: string }): void {
  p_keyed_default(topic);
}

function member(opts: { topic: string }): void {
  p_member(opts.topic);
}

function defaulted(name: string = "fallback"): void {
  p_defaulted(name);
}

function pair(prefix: string, name: string): void {
  p_pair(`${prefix}:${name}`);
}

function uncalled(name: string): void {
  p_uncalled(name);
}

function outer(name: string): void {
  p_outer(name);
}

function middle(name: string): void {
  outer(name);
}

function loop(name: string): void {
  p_loop(name);
  loop(name);
}

function ping(name: string): void {
  p_ping(name);
  pong(name);
}

function pong(name: string): void {
  ping(name);
}

function joinParts(name: string): void {
  p_joined([name, "x"].join(":"));
}

function lookup(kind: string): void {
  p_lookup(TABLE[kind]);
}

function grow(name: string): void {
  p_grow(name);
  grow(name + "/x");
}

function written(name: string): void {
  name = "changed";
  p_written(name);
}

function runtime(name: string): void {
  p_runtime(name);
}

class Bus {
  send(topic: string): void {
    p_send(topic);
  }
}

one("order.created");
two("order.created");
two("order.updated");
two(...names);
keyed({ topic: "jobs.run" });
keyedDefault({});
member({ topic: "member.topic" });
defaulted();
defaulted("given");
pair("a", "x");
pair("b", "y");
middle("deep");
["a"].forEach((item) => p_callback(item));
loop("once");
ping("m");
joinParts("a");
lookup("a");
grow("seed");
written("w");
new Bus().send("bus.sent");
runtime(argv[2]);
runtime(String(now()));

// A parameter and a `const` that share a line and a name.
const list = [{ id: "i" }];
const picked = list.find((picked) => picked.id === "i");
