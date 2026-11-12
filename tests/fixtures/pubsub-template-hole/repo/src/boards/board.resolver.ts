import { Arg, Mutation, Resolver, Subscription } from "../framework";
import { BusService } from "../events/bus.service";
import { BoardService } from "./board.service";

@Resolver()
export class BoardResolver {
  constructor(
    private readonly boards: BoardService,
    private readonly bus: BusService,
  ) {}

  @Mutation()
  async reorderBoard(
    @Arg("boardId") boardId: string,
    @Arg("parentId") parentId: string | null,
  ) {
    return this.boards.reorder(boardId, parentId);
  }

  @Mutation()
  async createBoard(@Arg("title") title: string) {
    return this.boards.create(title);
  }

  @Mutation()
  async archiveBoard() {
    return this.boards.archive();
  }

  @Subscription()
  childReordered(@Arg("boardId") boardId: string) {
    return this.bus.listen(`board_child/${boardId}/reordered`);
  }
}
