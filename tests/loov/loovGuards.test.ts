import { describe, expect, it } from "vitest";
import { filterLoovEnvConflicts } from "@/lib/loovManaged";

const c = (varName: string) => ({ varName, varValue: "x", sourceType: "file" as const, sourcePath: "~/.bashrc:1" });

describe("filterLoovEnvConflicts", () => {
  const all = [
    c("ANTHROPIC_VERTEX_PROJECT_ID"),
    c("ANTHROPIC_API_KEY"),
    c("ANTHROPIC_BASE_URL"),
    c("ANTHROPIC_DEFAULT_OPUS_MODEL"),
    c("OPENAI_BASE_URL"),
    c("GEMINI_API_KEY"),
    c("ANTHROPIC_CUSTOM_HEADERS"),
  ];

  it("未接管时只去掉员工自己的云厂商变量", () => {
    expect(filterLoovEnvConflicts(all, false).map((x) => x.varName)).toEqual(
      all.map((x) => x.varName).filter((n) => n !== "ANTHROPIC_VERTEX_PROJECT_ID"),
    );
    expect(filterLoovEnvConflicts([c("ANTHROPIC_BEDROCK_BASE_URL"), c("ANTHROPIC_FOUNDRY_API_KEY")], false)).toEqual([]);
  });

  it("接管时只保留仍会影响链路的变量", () => {
    expect(filterLoovEnvConflicts(all, true).map((x) => x.varName)).toEqual(["ANTHROPIC_CUSTOM_HEADERS"]);
  });
});

describe("isLoovOwnedMcp", () => {
  it("只认带 loov 标签的条目", async () => {
    const { isLoovOwnedMcp } = await import("@/components/loov/LoovMcpGuard");
    expect(isLoovOwnedMcp({ tags: ["loov"] })).toBe(true);
    expect(isLoovOwnedMcp({ tags: ["search"] })).toBe(false);
    expect(isLoovOwnedMcp({})).toBe(false);
    expect(isLoovOwnedMcp({ tags: "loov" })).toBe(false);
  });
});
