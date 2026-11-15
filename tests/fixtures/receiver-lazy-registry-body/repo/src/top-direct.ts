import router from "fixture-router";

// G: a destructured key of a module loaded directly at module scope.
const app = router();
const { default: init } = await import("./svc/g");
init(app);
