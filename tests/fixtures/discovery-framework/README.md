# Discovery fixture: routes on a framework instance parameter (carrick#2725)

`repo/` is one service (`services/api`) whose route module receives the
server framework's instance as a typed parameter and registers two routes on
it: one with an inline handler, one whose handler is a call into a local
wrapper. The module imports the framework as a type only (since carrick#2588
the service still asks the store about it, through the compiler's call rows);
the app that builds the instance and hands it to `register` lives in another
service.

`library-claims.json` is a store answer that holds the framework anyway (as
a replay cassette that answers every package does), labelled
`server_framework` with no op claims, as the store labels it today.

The test (`fact_files.rs`) reads the two registrations through the first
answer this service gets: no export of the framework (the store has not
answered it yet), and the framework not among the packages answered.
Discovery must record both as not followed (`unanswered_package`), never as
a finished walk, or D4 drops the register signal's candidates.
