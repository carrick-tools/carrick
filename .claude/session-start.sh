#!/bin/sh
# Prints one line of context at the start of the session. No state, no
# branching: the same bytes every time it runs, and nothing that can fail.
printf %s 'Your working directory is on disk and holds the code this session is about; `ls` it first, then read carrick.json in the repo you are asked about. Then, before other work in this session, call mcp__carrick__get_project_map with project: "scanner-evals" and service set to this repo, taking the name from carrick.json or the repo itself. The map names the sibling services this project indexes, the contracts crossing into and out of this repo, and the drill-down tool for each section. The map is the view from the index; every path it names is a file in one of the repos in this project. Its header states the commit each repo was indexed at: `git diff --name-only <that commit>` in the checkout lists the files the index is silent about. Open the file at a `file_location` before you state what it contains.'
exit 0
