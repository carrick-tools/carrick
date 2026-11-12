import { Injectable } from "../framework";
import { BusService } from "../events/bus.service";

function newBoardId(): string {
  return Math.random().toString(36).slice(2);
}

@Injectable()
export class BoardService {
  constructor(private readonly bus: BusService) {}

  async reorder(boardId: string, parentId: string | null) {
    if (!parentId) {
      this.bus.publish(`board_root/${boardId}/reordered`, true);
    } else {
      this.bus.publish(
        `board_child/${boardId}/reordered`,
        parentId,
      );
    }
    return true;
  }

  async create(title: string) {
    await this.bus.publish("board/created", { title });
    return title;
  }

  async archive() {
    const boardId = newBoardId();
    await this.bus.publish(`board/${boardId}/archived`, true);
    return boardId;
  }
}
