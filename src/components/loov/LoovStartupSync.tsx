/**
 * loov: Loov 启动要先建隧道（走本机代理时要几秒），上游的本地路由在那之后才启动。上游的路由状态查询
 * 只在「运行中」时轮询，首查拿到「未运行」就不再刷新，界面会一直误报「路由服务没有运行」。
 * Loov 状态离开 starting 时让全部查询失效重查（只发生一次；侧栏的路由红点等也一并刷新）。
 */
import { useEffect, useRef } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useLoovStatus } from "@/lib/query/loov";

export function LoovStartupSync() {
  const queryClient = useQueryClient();
  const { data: status } = useLoovStatus();
  const prev = useRef<string | undefined>(undefined);
  useEffect(() => {
    const state = status?.state;
    // 启动时连好，或刚登录 / 重新登录（从未登录变成已连接）：后端刚写了供应商、进了聚合模式，
    // 之前缓存的供应商列表（登录前是空的）都要重新取
    const loggedIn = prev.current === "noCredentials" || prev.current === "reloginRequired";
    if ((prev.current === "starting" && state !== "starting") || (loggedIn && state === "ready")) {
      void queryClient.invalidateQueries();
    }
    prev.current = state;
  }, [status?.state, queryClient]);
  return null;
}
