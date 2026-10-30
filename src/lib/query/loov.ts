import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { loovApi } from "@/lib/api/loov";

export const loovKeys = {
  status: ["loov", "status"] as const,
  session: ["loov", "session"] as const,
  account: ["loov", "account"] as const,
  devices: ["loov", "devices"] as const,
  meta: ["loov", "meta"] as const,
  managed: ["loov", "managed"] as const,
  apps: ["loov", "apps"] as const,
  usage: (range: "today" | "month") => ["loov", "usage", range] as const,
  prefs: ["loov", "prefs"] as const,
};

export function useLoovAppVersion() {
  return useQuery({ queryKey: ["loov", "appVersion"], queryFn: loovApi.appVersion, staleTime: Infinity, retry: 0 });
}

export function useLoovManaged() {
  return useQuery({ queryKey: loovKeys.managed, queryFn: loovApi.managed });
}

export function useLoovStatus() {
  return useQuery({
    queryKey: loovKeys.status,
    queryFn: loovApi.status,
    // 启动建隧道期间每秒查一次，就绪后回到 10 秒
    refetchInterval: (query) => (query.state.data?.state === "starting" ? 1_000 : 10_000),
  });
}

export function useLoovSession(enabled: boolean) {
  return useQuery({ queryKey: loovKeys.session, queryFn: loovApi.session, enabled });
}

/** 花费统计：hub 侧按 Key 查 new-api 日志并缓存 1 分钟，这里 60 秒刷新一次 */
export function useLoovUsage(range: "today" | "month", enabled: boolean) {
  return useQuery({
    queryKey: loovKeys.usage(range),
    queryFn: () => loovApi.usage(range),
    enabled,
    refetchInterval: 60_000,
    retry: 1,
  });
}

export function useLoovPrefs() {
  return useQuery({ queryKey: loovKeys.prefs, queryFn: loovApi.prefs });
}

export function useLoovAccount(enabled: boolean) {
  return useQuery({
    queryKey: loovKeys.account,
    queryFn: loovApi.account,
    enabled,
    refetchInterval: 30_000,
    retry: 1,
  });
}

export function useLoovDevices(enabled: boolean) {
  return useQuery({ queryKey: loovKeys.devices, queryFn: loovApi.devices, enabled });
}

export function useLoovApps(enabled: boolean) {
  return useQuery({ queryKey: loovKeys.apps, queryFn: loovApi.appsStatus, enabled, staleTime: 30_000 });
}

export function useLoovMeta() {
  return useQuery({ queryKey: loovKeys.meta, queryFn: loovApi.meta, staleTime: 5 * 60_000, retry: 0 });
}

export function useInvalidateLoov() {
  const qc = useQueryClient();
  return () => {
    void qc.invalidateQueries({ queryKey: ["loov"] });
  };
}

export function useLoovLogout() {
  const invalidate = useInvalidateLoov();
  return useMutation({ mutationFn: loovApi.logout, onSettled: invalidate });
}
