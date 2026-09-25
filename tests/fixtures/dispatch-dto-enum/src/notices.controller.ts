import { CreateNoticeDto } from "./notices.dto";
import { NoticesService } from "./notices.service";

export class NoticesController {
  constructor(private readonly notices: NoticesService) {}

  async create(body: CreateNoticeDto): Promise<string[]> {
    return this.notices.create(body);
  }
}
