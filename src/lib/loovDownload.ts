import { loovApi } from "@/lib/api/loov";

// loov: 客户端下载 / 更新日志统一走 hub /meta 的 downloadUrl，不再指向上游发布页
export const LOOV_DEFAULT_DOWNLOAD_URL = "https://me.loov.cc";

const META_TIMEOUT_MS = 5000;

/** hub 不可达、超时或未配置 downloadUrl 时返回 {@link LOOV_DEFAULT_DOWNLOAD_URL}。 */
export async function resolveLoovDownloadUrl(): Promise<string> {
  try {
    const meta = await Promise.race([
      loovApi.meta(),
      new Promise<null>((resolve) =>
        setTimeout(() => resolve(null), META_TIMEOUT_MS),
      ),
    ]);
    const url = meta?.client?.downloadUrl?.trim();
    if (url && /^https?:\/\//i.test(url)) return url;
  } catch (error) {
    console.warn("[loov] Failed to load download url from hub meta", error);
  }
  return LOOV_DEFAULT_DOWNLOAD_URL;
}
