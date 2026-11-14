// carrick#4165: a call that writes nothing at the label's payload argument
// sends no body there, read up the calls to the one that decides the site. A
// wrapper parameter with a default is filled by it, and a spread may fill any
// position: neither is read as absent.

export function ping(): Promise<Response> {
  return fetch(`${process.env.API}/absent/ping`);
}

// The labelled call inside the wrapper writes no body: no caller can.
function bump(id: string) {
  return fetch(`${process.env.API}/absent/${id}/bump`);
}

export function bumpOne(): Promise<Response> {
  return bump("1");
}

// The wrapper hands its own optional parameter on, and the caller leaves it
// out.
function carry(path: string, body?: unknown) {
  return fetch(`${process.env.API}${path}`, body as RequestInit);
}

export function carried(): Promise<Response> {
  return carry("/absent/carried");
}

// Controls: the parameter's default fills it, also under a wrapper that
// hands on a parameter with none, and a spread may.
function send(url: string, init: unknown = {}) {
  return fetch(`${process.env.API}${url}`, init as RequestInit);
}

export function defaulted(): Promise<Response> {
  return send("/absent/defaulted");
}

function relay(url: string, init?: unknown) {
  return send(url, init);
}

export function relayed(): Promise<Response> {
  return relay("/absent/relayed");
}

// A body the wrapper assigns again, and a bind that fills the leading
// parameter: the caller's own arguments are not the callee's positions.
function reset(path: string, init?: unknown) {
  init = init ?? { body: "x" };
  return fetch(`${process.env.API}${path}/reset`, init as RequestInit);
}

export function resetCaller(): Promise<Response> {
  return reset("/absent");
}

const boundCarry = carry.bind(null, "/absent/bound");

export function bound(): Promise<Response> {
  return boundCarry({ body: "x" });
}

export function spread(rest: [RequestInit]): Promise<Response> {
  return fetch(`${process.env.API}/absent/spread`, ...rest);
}

export function options(): Promise<Response> {
  return fetch(`${process.env.API}/absent/options`, { headers: {} });
}

// A wrapper that writes the body onto the options its caller hands it: the
// caller's literal lacks `body`, yet the request carries one (carrick#4324).
function stamp(path: string, init: RequestInit) {
  init.body = "x";
  return fetch(`${process.env.API}${path}`, init);
}

export function stamped(): Promise<Response> {
  return stamp("/absent/stamped", { method: "POST" });
}

// Control: a wrapper that only hands the options on writes nothing onto them.
function forward(path: string, init: RequestInit) {
  return fetch(`${process.env.API}${path}/forwarded`, init);
}

export function forwarded(): Promise<Response> {
  return forward("/absent/forwarded", { method: "POST" });
}

// A wrapper that hands the options to a helper before it sends them: the
// helper may write the body onto them.
function addBody(init: RequestInit) {
  init.body = "x";
}

function helped(path: string, init: RequestInit) {
  addBody(init);
  return fetch(`${process.env.API}${path}/helped`, init);
}

export function helpedCaller(): Promise<Response> {
  return helped("/absent/helped", { method: "POST" });
}

// carrick#4294: a closed options literal below the deciding call lacks the
// body key: the call sends none.
function wipe(path: string) {
  return fetch(`${process.env.API}${path}/wiped`, { method: "DELETE", headers: {} });
}

export function wiped(): Promise<Response> {
  return wipe("/absent/wiped");
}

// Controls: a literal that spreads its caller's options, options built in a
// local a branch may extend, and a literal handed to a hop that writes onto it.
function wipeWith(path: string, extra: RequestInit) {
  return fetch(`${process.env.API}${path}/wipe-with`, { method: "DELETE", ...extra });
}

export function wipedWith(): Promise<Response> {
  return wipeWith("/absent/wipe-with", { body: "x" });
}

function wipeBuilt(path: string, text?: string) {
  const init: RequestInit = { method: "DELETE" };
  if (text) init.body = text;
  return fetch(`${process.env.API}${path}/wipe-built`, init);
}

export function wipedBuilt(): Promise<Response> {
  return wipeBuilt("/absent/wipe-built");
}

function wipeStamped(path: string) {
  return stamp(path, { method: "DELETE" });
}

export function wipedStamped(): Promise<Response> {
  return wipeStamped("/absent/wipe-stamped");
}
