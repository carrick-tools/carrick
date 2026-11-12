// Preloaded into a hook child by tests (`--import`): every `fs` read or open of
// a path containing CARRICK_TEST_READS_MATCH is logged to CARRICK_TEST_READS_LOG.
import fs from "node:fs";

const match = process.env["CARRICK_TEST_READS_MATCH"];
const log = process.env["CARRICK_TEST_READS_LOG"];
if (match && log) {
  const append = fs.appendFileSync;
  for (const name of ["readFileSync", "openSync", "readFile", "open", "createReadStream"]) {
    const original = fs[name];
    fs[name] = function (target, ...rest) {
      if (String(target).includes(match)) append(log, `${name} ${String(target)}\n`);
      return original.call(this, target, ...rest);
    };
  }
}
