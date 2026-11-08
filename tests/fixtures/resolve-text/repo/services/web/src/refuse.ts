export {};

let WRITTEN: string = "/before";
WRITTEN = "/after";
const WRITTEN_ENTRY = { a: "/a" };
WRITTEN_ENTRY.a = "/b";
export const EXPORTED = { a: "/a" };
var OLD: string = "/old";
const PUSHED = { list: ["x"], a: "/a" };
PUSHED.list.push("y");

const p_written = WRITTEN;
const p_written_entry = WRITTEN_ENTRY.a;
const p_exported = EXPORTED.a;
const p_var = OLD;
const p_pushed = PUSHED.a;
const p_dollar = "/x/y".replace("x", "$&");
const p_param = (name: string): string => `/${name}`;
