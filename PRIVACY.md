# Privacy

## Usage counts

Carrick counts how often its hooks run on your machine and sends the daily totals to Carrick. It is on by default, after you have been told once (by `carrick init`, or by a notice at your next Claude Code session start).

**What is sent**, per day and per hook (session start, read, edit, shell, re-check, prompt, stop):

- how many times the hook ran, and how many of those printed text to your agent, stayed silent or failed
- the median and 90th-percentile run time, and the median time spent waiting on a check
- the Carrick version, a random id for this machine and a random id for this folder, and your Carrick workspace

**What is never sent:** code, file names or paths, repository, service or branch names, host or user names, session ids, environment values, or anything an agent wrote or read.

The counts say that a hook printed text, not which text and not whether your agent used it. The exact body of the last send is in `.carrick/usage-last.json` in your folder, and `carrick telemetry status` prints it.

**Where it goes:** Carrick's own log store in the EU (eu-central-1). It is not shared with anyone else.

**Turn it off:** `carrick telemetry off`, or set `CARRICK_TELEMETRY=0` or `DO_NOT_TRACK=1`. Nothing is sent when `CI` is set. `carrick uninstall` removes the setting and the machine id.
