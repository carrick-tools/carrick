#!/usr/bin/env node
// A stand-in for a scan that is told to stop and has something to do about it.
//
// The real one hears the signal, tells Carrick Cloud its scan stopped and
// ships its log, which takes a moment and is the whole reason the parent has
// to stay for it (carrick#1235, carrick#1391). This does the same shape: it
// records the signal it was sent, takes a moment over it, and exits with the
// code a signalled run exits with.
//
// Env it honours:
//   CARRICK_FAKE_SIGNAL_FILE  where to write the signal it was sent

import fs from "node:fs";
import process from "node:process";

const record = process.env.CARRICK_FAKE_SIGNAL_FILE;

for (const signal of ["SIGTERM", "SIGHUP", "SIGINT"]) {
  process.on(signal, () => {
    if (record) fs.writeFileSync(record, signal);
    setTimeout(() => process.exit(signal === "SIGTERM" ? 143 : 129), 200);
  });
}

// On disk rather than on a stream: the parent renders this one rather than
// passing it through, so a test waiting on the parent's own stderr waits for
// something that only arrives when the run is over.
if (record) fs.writeFileSync(`${record}.ready`, "up");
process.stderr.write("@carrick-phase {\"label\":\"indexing api\",\"state\":\"started\"}\n");
// Long enough that nothing here ends on its own inside a test.
setInterval(() => {}, 60000);
