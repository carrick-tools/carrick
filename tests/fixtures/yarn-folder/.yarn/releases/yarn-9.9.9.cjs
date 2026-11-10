const { EventEmitter } = require("events");
const progress = new EventEmitter();
progress.on("fetchStep", (n) => {});
function reportStep(n) {
  progress.emit("fetchStep", n);
}
module.exports = { reportStep };
