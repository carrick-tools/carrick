import { Live } from "@fixture/live-wire";
import { announce } from "./announce";
import { listen } from "./twin";

announce(new Live("wss://live.example/ws"), "hello");
listen(new Live("wss://live.example/ws"));
