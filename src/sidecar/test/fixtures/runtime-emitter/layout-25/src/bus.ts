/// <reference types="node" />
import { EventEmitter } from "node:events";
import { EventEmitter as Plain } from "fixture-emitter";

export const bus = new EventEmitter();
export const plain = new Plain();
