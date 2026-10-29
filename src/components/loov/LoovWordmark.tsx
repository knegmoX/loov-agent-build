import { cn } from "@/lib/utils";

/**
 * 多色字标 LoOVageNt，与 web（me.loov.cc 首页、下载页、控制台）同一套写法：
 * 逐字着色（颜色是 index.css 的 --w-*，深浅色各一套），字号由 className 决定。
 */
export function LoovWordmark({ className }: { className?: string }) {
  return (
    <span className={cn("loov-wm", className)} role="img" aria-label="Loov Agent">
      <span className="l">L</span>
      <span className="o">o</span>
      <span className="O">O</span>
      <span className="v">V</span>
      <span className="t">age</span>
      <span className="n">N</span>
      <span className="t">t</span>
    </span>
  );
}
