import router from "fixture-router";
import services from "./services";

declare const process: { argv: string[] };

// Which module a name loads is a choice made at run time: the callee of
// `init` is the default export of whichever thunk's module the key names.
const name = process.argv[2] as keyof typeof services;
const app = router();
const { default: init } = await services[name]();
init(app);
