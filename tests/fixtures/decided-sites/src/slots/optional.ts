import { connect } from "@fixture/wire";

const wire = connect("ws://x");

export function sendPlain(): void {
  wire.emit("tick", { at: 1 });
}

export function sendOptional(): void {
  wire?.emit("tick", { at: 1 });
}

export function listenOptional(): void {
  wire?.on("user.joined", (data) => {
    console.log(data);
  });
}
