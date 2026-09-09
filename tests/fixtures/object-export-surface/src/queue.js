// The other CommonJS shape: the whole module surface assigned at once
// (carrick#863). `module.exports = { … }` is the default export, so its members
// key the same way `export default { … }` does.

async function consume(message) {
  return message.body;
}

module.exports = {
  // A declared function offered by name: one definition, at its own key.
  consume,
  // A method written in the object, which exists nowhere else.
  async drain(queue) {
    return queue.length;
  },
};
