import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

// 与 Rust core::config::Config（serde camelCase）严格一致
interface Config {
  notifyEnabled: boolean;
  runningFreshMs: number;
  doneAfterMs: number;
}

// 与 lib.rs get_source_status 的 json! 键一致
interface SourceStatus {
  claude_hooks: boolean;
  claude_transcripts: boolean;
  zcode_db: boolean;
}

const MS_PER_MIN = 60_000;

function SourceDot({ ok, label }: { ok: boolean | undefined; label: string }) {
  return (
    <div className="src-row">
      <span className={`dot ${ok ? "dot-ok" : "dot-off"}`} />
      <span>
        {label}
        {ok === undefined ? "…" : ok ? "" : "（未就绪）"}
      </span>
    </div>
  );
}

export default function Settings({ onBack }: { onBack: () => void }) {
  const [status, setStatus] = useState<SourceStatus | null>(null);
  const [cfg, setCfg] = useState<Config | null>(null);
  const [savedTip, setSavedTip] = useState(false);
  const [installMsg, setInstallMsg] = useState<string | null>(null);
  const [installErr, setInstallErr] = useState<string | null>(null);

  useEffect(() => {
    invoke<SourceStatus>("get_source_status").then(setStatus);
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
            ? `已写入 hook，备份：${r.backupPath}`
            : "hook 已注册过，无需重复接入",
        ),
      )
      .catch((e) => setInstallErr(String(e)));
  };

  return (
    <div className="settings">
      <button className="chip" onClick={onBack}>← 返回看板</button>

      <section>
        <h2>数据源</h2>
        <SourceDot ok={status?.claude_hooks} label="Claude Code hook" />
        <SourceDot ok={status?.claude_transcripts} label="Claude 转录目录" />
        <SourceDot ok={status?.zcode_db} label="ZCode 数据库" />
        <button className="btn" onClick={installHooks}>接入 Claude Code</button>
        {installMsg && <div className="tip">{installMsg}</div>}
        {installErr && <div className="err">{installErr}</div>}
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
