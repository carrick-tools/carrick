// The CommonJS export surface, in its named forms (carrick#863). This is how
// every `.js` lambda states its entry point, and none of these shapes is a
// declaration, a function-initialised variable or a class member.

// The named export, arrow-valued: a lambda entry point.
exports.handler = async (event) => {
  return { statusCode: 200, body: JSON.stringify(event) };
};

// The same, written through `module.exports`, with a named function
// expression. The name on the expression is not a declaration, so nothing else
// records it.
module.exports.health = function health(deep) {
  return deep ? "deep" : "ok";
};

// Declared in the module and offered under a table below. It has a definition
// at its own key already; what the table adds is that the module offers it, so
// there is no second row for the same body.
function reset(id) {
  return `reset:${id}`;
}

// The bound: a module-local function that no assignment reaches stays local.
function sweep(value) {
  return value.trim();
}

module.exports.table = { reset, drop: (id) => `drop:${id}` };

// Keeps `sweep` from reading as dead code to a linter without exporting it.
reset(sweep("x"));
