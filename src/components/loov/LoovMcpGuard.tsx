/**
 * loov: MCP 面板里 Loov 托管条目（云端搜索 loov-search）的判断与标记。
 * 放在 Loov 目录，上游 UnifiedMcpPanel.tsx 只留单行挂载点，合并上游时冲突面最小（审计 S6-02）。
 */
import { useTranslation } from "react-i18next";
import { NeutralBadge } from "@/components/mcp/AppMatrix";

export function isLoovOwnedMcp(server: { tags?: unknown }): boolean {
  return Array.isArray(server.tags) && server.tags.includes("loov");
}

export function LoovManagedBadge() {
  const { t } = useTranslation();
  return <NeutralBadge>{t("loov.mcpManaged", { defaultValue: "Loov 托管" })}</NeutralBadge>;
}
