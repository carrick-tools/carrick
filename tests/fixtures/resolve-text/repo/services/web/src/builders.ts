// Names an own builder returns (carrick#3815). Each probe is a call of a
// `p_build_*` function whose argument is a call of a builder written in this
// file; the functions that make the probe calls sit below the builders and
// are called at the bottom, so the test finds a probe by its callee name.
// Every parameter is typed `string`: a literal type would be read from the
// type first.
export {};

declare function p_build_lit(name: string): void;
declare function p_build_param(name: string): void;
declare function p_build_local(name: string): void;
declare function p_build_member(name: string): void;
declare function p_build_nested(name: string): void;
declare function p_build_keyed(name: string): void;
declare function p_build_async(name: string): void;
declare function p_build_self(name: string): void;

const PREFIX = "svc";

// An arrow whose body is the text.
const topicFor = (kind: string): string => `${PREFIX}.${kind}`;

// A function that declares a constant first and returns it.
function twoSteps(name: string): string {
  const topic = `two.${name}`;
  return topic;
}

// A builder written as an entry of a constant object.
const ROUTES = { byId: (id: string): string => `jobs.${id}` };

// A builder that calls another with its own parameter.
function nested(name: string): string {
  return topicFor(`n.${name}`);
}

// A builder that reads a key of its object parameter.
function keyedTopic(opts: { area: string }): string {
  return `area.${opts.area}`;
}

// An async builder: its call is a promise, never the text.
async function later(name: string): Promise<string> {
  return `later.${name}`;
}

// A builder that calls itself.
function again(name: string): string {
  return again(name);
}

// Two calls of one builder in one function: one with a literal, one handed
// the function's own parameter, filled by its one caller.
function both(kind: string): void {
  p_build_lit(topicFor("x"));
  p_build_param(topicFor(kind));
}

function others(): void {
  p_build_local(twoSteps("z"));
  p_build_member(ROUTES.byId("7"));
  p_build_nested(nested("q"));
  p_build_keyed(keyedTopic({ area: "west" }));
  p_build_async(later("l"));
  p_build_self(again("s"));
}

both("filled");
others();
