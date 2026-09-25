// Preloaded into a hook process by the 300 ms budget test (carrick#1498).
//
// Reports the CPU time the hook's main thread spent, as one line on stderr
// when it exits. That is where the hook's own work runs: loading and stripping
// its modules, parsing the payload, rendering the answer. It leaves out the
// CLI the hook spawns (another process) and V8's background compiler and GC
// threads, whose CPU grows with how busy the machine is rather than with what
// the hook does.
//
// CPU time rather than wall time because `node --test` runs test files
// concurrently: on a shared runner a hook's wall clock measures its neighbours
// as much as itself, and an unrelated PR failed at 308 ms that way. On a
// laptop the figure is about 60 ms idle and 90 ms with every core
// oversubscribed three times, so 300 ms leaves room for a slower runner core
// and still fails a hook that takes on real work.
import fs from "node:fs";

// A synchronous write: an exit handler cannot wait, and stderr is an
// asynchronous pipe on macOS.
process.on("exit", () => {
  const { user, system } = process.threadCpuUsage();
  fs.writeSync(2, `carrick-test-cpu-ms=${Math.round((user + system) / 1000)}\n`);
});
