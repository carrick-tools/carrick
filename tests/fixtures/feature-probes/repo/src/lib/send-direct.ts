import { send } from '@fixture/send';
// A direct call of `send`: without one, a package read only as a value gets
// no label applied (carrick#3558), and `t-factory-closure-param` is not read.
export function sendDirect() { return send('/p/lib-send-direct'); }
