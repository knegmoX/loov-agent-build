/**
 * agent 权限档位：Loov 不做自己的沙箱，只把这一个选择写成各家 agent 自己的原生设置
 * （Claude Code permissions.defaultMode、Codex approval_policy / sandbox_mode、Grok permission_mode、
 * agy 启动参数，见 src-tauri/src/loov/safety.rs）。员工在 agent 里自己改过的值不会被覆盖。
 */
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useQueryClient } from "@tanstack/react-query";
import { SegmentedControl } from "@/components/ui/segmented-control";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { toast } from "@/lib/toast";
import { loovApi, parseLoovError, type LoovSafetyLevel } from "@/lib/api/loov";
import { loovKeys, useLoovPrefs } from "@/lib/query/loov";

export function SafetyLevel() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const { data: prefs } = useLoovPrefs();
  const [ask, setAsk] = useState(false);
  const level = prefs?.safety ?? "standard";

  const save = async (next: LoovSafetyLevel) => {
    try {
      await loovApi.setSafety(next);
      await qc.invalidateQueries({ queryKey: loovKeys.prefs });
      toast.success(t("loov.safety.saved"));
    } catch (e) {
      toast.error(parseLoovError(e).message);
    }
  };

  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5 px-1">
      <span className="text-xs text-fg-2">{t("loov.safety.label")}</span>
      <SegmentedControl<LoovSafetyLevel>
        size="sm"
        aria-label={t("loov.safety.label")}
        value={level}
        onValueChange={(next) => {
          if (next === level) return;
          if (next === "full") setAsk(true);
          else void save(next);
        }}
        items={(["careful", "standard", "full"] as const).map((v) => ({ value: v, label: t(`loov.safety.levels.${v}`) }))}
      />
      <span className="text-caption text-fg-3">{t(`loov.safety.hint.${level}`)}</span>

      <Dialog open={ask} onOpenChange={setAsk}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t("loov.safety.fullTitle")}</DialogTitle>
            <DialogDescription>{t("loov.safety.fullDesc")}</DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="neutral" onClick={() => setAsk(false)}>
              {t("common.cancel")}
            </Button>
            <Button
              variant="solid"
              onClick={() => {
                setAsk(false);
                void save("full");
              }}
            >
              {t("loov.safety.fullOk")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
