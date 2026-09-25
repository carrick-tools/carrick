import { CreateNoticeDto } from "./notices.dto";

export class NoticesService {
  async create(dto: CreateNoticeDto): Promise<string[]> {
    const recipients = dto.scope === "all" ? ["everyone"] : (dto.userIds ?? []);
    await fetch("https://mail.example.com/v1/send", {
      method: "POST",
      body: JSON.stringify({ to: recipients, text: dto.body }),
    });
    return recipients;
  }
}
