import * as readline from "node:readline";

// Reads one request per line from the parent process.
const reader = readline.createInterface({ input: process.stdin });

reader.on("line", (line: string) => {
  console.log(line.length);
});
