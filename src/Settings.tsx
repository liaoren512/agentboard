import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

// 与 Rust core::config::Config（serde camelCase）严格一致
interface Config {
  notifyEnabled: boolean;
  runningFreshMs: number;
  doneAfterMs: number;
}

// 与 lib.rs get_source_status 的 json! 结构一致（每源 { connected, last_event_at }）
interface SourceInfo {
  connected: boolean;
  last_event_at: number | null;
}
interface SourceStatus {
  claude_transcripts: SourceInfo;
  claude_events: SourceInfo;
  zcode: SourceInfo;
}

const MS_PER_MIN = 60_000;
const STALE_MS = 10 * 60 * 1000; // 与 lib.rs SOURCE_STALE_MS 一致

// 与 lib.rs health_state 同规则：无数据→未接入 / 10 分钟内→正常 / 超时→需要检查
type HealthState = "none" | "ok" | "stale";
function healthState(last: number | null, now: number): HealthState {
  if (last === null) return "none";
  return now - last < STALE_MS ? "ok" : "stale";
}

function agoText(ts: number): string {
  const diff = Math.max(0, Date.now() - ts);
  if (diff < 60_000) return `${Math.floor(diff / 1000)} 秒前`;
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)} 分钟前`;
  return `${Math.floor(diff / 3_600_000)} 小时前`;
}

function healthText(state: HealthState, last: number | null): string {
  if (state === "ok") return `正常 · 最后数据 ${agoText(last as number)}`;
  if (state === "stale") return "需要检查 · 超过 10 分钟无数据";
  return "未接入";
}

function SourceRow({
  label,
  info,
  button,
}: {
  label: string;
  info: SourceInfo | undefined;
  button?: React.ReactNode;
}) {
  const state: HealthState = info ? healthState(info.last_event_at, Date.now()) : "none";
  const cls = state === "ok" ? "dot-ok" : state === "stale" ? "dot-check" : "dot-off";
  return (
    <div className="src-row">
      <span className={`dot ${cls}`} />
      <span>
        {label} {info ? healthText(state, info.last_event_at) : "…"}
      </span>
      {button}
    </div>
  );
}

export default function Settings({ onBack }: { onBack: () => void }) {
  const [status, setStatus] = useState<SourceStatus | null>(null);
  const [cfg, setCfg] = useState<Config | null>(null);
  const [savedTip, setSavedTip] = useState(false);
  const [installMsg, setInstallMsg] = useState<string | null>(null);
  const [installErr, setInstallErr] = useState<string | null>(null);
  const [zcMsg, setZcMsg] = useState<string | null>(null);
  const [zcErr, setZcErr] = useState<string | null>(null);

  const refresh = () => {
    invoke<SourceStatus>("get_source_status").then(setStatus);
  };

  useEffect(() => {
    refresh();
    invoke<Config>("get_config").then(setCfg);
  }, []);

  const save = (next: Config) => {
    setCfg(next);
    setSavedTip(false);
    invoke("set_config", { cfg: next })
      .then(() => setSavedTip(true))
      .catch((e) => console.warn("set_config failed:", e));
  };

  const installHooks = () => {
    setInstallMsg(null);
    setInstallErr(null);
    invoke<{ backupPath: string; changed: boolean }>("install_hooks_cmd")
      .then((r) =>
        setInstallMsg(
          r.changed
            ? r.backupPath
              ? `已写入 hook，备份：${r.backupPath}`
              : "已写入 hook（新文件，无备份）"
            : "hook 已注册过，无需重复接入",
        ),
      )
      .catch((e) => setInstallErr(String(e)));
  };

  const installZcodeHooks = () => {
    setZcMsg(null);
    setZcErr(null);
    invoke<{ backupPath: string; changed: boolean }>("install_zcode_hooks_cmd")
      .then((r) => {
        setZcMsg(
          r.changed
            ? r.backupPath
              ? `已写入 ZCode hooks，备份：${r.backupPath}`
              : "已写入 ZCode hooks（新文件，无备份）"
            : "ZCode hooks 已注册过，无需重复接入",
        );
        refresh();
      })
      .catch((e) => setZcErr(String(e)));
  };

  return (
    <div className="settings">
      <button className="chip" onClick={onBack}>← 返回看板</button>

      <section>
        <h2>数据源</h2>
        <SourceRow label="Claude 转录" info={status?.claude_transcripts} />
        <SourceRow label="Claude hooks 事件" info={status?.claude_events} />
        <SourceRow
          label="ZCode"
          info={status?.zcode}
          button={
            <button className="btn-inline" onClick={installZcodeHooks}>
              接入 ZCode hooks
            </button>
          }
        />
        <button className="btn" onClick={installHooks}>接入 Claude Code</button>
        {installMsg && <div className="tip">{installMsg}</div>}
        {installErr && <div className="err">{installErr}</div>}
        {zcMsg && <div className="tip">{zcMsg}</div>}
        {zcErr && <div className="err">{zcErr}</div>}
      </section>

      {cfg && (
        <section>
          <h2>通知与阈值</h2>
          <div className="setting-row">
            <label>
              <input
                type="checkbox"
                checked={cfg.notifyEnabled}
                onChange={(e) => save({ ...cfg, notifyEnabled: e.target.checked })}
              />
              桌面通知
            </label>
          </div>
          <div className="setting-row">
            <label>
              运行中视为新鲜的窗口
              <input
                type="number" min={1}
                value={cfg.runningFreshMs / MS_PER_MIN}
                onChange={(e) => save({ ...cfg, runningFreshMs: Math.max(1, Number(e.target.value) || 0) * MS_PER_MIN })}
              />
              分钟
            </label>
          </div>
          <div className="setting-row">
            <label>
              静默多久推断完成
              <input
                type="number" min={1}
                value={cfg.doneAfterMs / MS_PER_MIN}
                onChange={(e) => save({ ...cfg, doneAfterMs: Math.max(1, Number(e.target.value) || 0) * MS_PER_MIN })}
              />
              分钟
            </label>
          </div>
          {savedTip && <div className="tip">已保存</div>}
        </section>
      )}
    </div>
  );
}
