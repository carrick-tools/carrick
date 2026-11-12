declare const require: any;
const { Client, Command } = require("sdk");
export async function declared() {
  await new Client().send(new Command({ Name: "declared" }));
}
