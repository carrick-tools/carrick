// The line `carrick init` prints about the hosted index, for an actual Rust
// `carrick status --json` payload. The native integration test supplies the
// payload; any other command init would run is a failure here, so the line is
// the one a run that re-read nothing prints.
import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const { status, root } = JSON.parse(fs.readFileSync(0, 'utf8'));
const npm = process.env.CARRICK_CONSUMER_SOURCE;
const { downloadHostedIndex, hostedReport } = await import(
  pathToFileURL(path.join(npm, 'src/init/hosted.ts')).href
);
const run = async (args) => {
  if (args[0] !== 'status') throw new Error(`init ran \`carrick ${args.join(' ')}\``);
  return { status: 0, stdout: JSON.stringify(status), stderr: '' };
};
const outcome = await downloadHostedIndex(root, run, status.repos.map((repo) => repo.repo));
process.stdout.write(`${hostedReport(outcome).text}\n`);
