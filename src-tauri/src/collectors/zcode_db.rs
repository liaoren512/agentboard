// ZCode db 只读采集器：轮询 ~/.zcode/cli/db/db.sqlite（WAL），把会话/存活/turn 状态
// 转成 UnifiedEvent 边沿事件（状态不变则不产事件）。
use crate::core::types::*;
use std::collections::HashMap;

#[derive(Clone)]
struct Row {
    title: String,
    directory: String,
    /// 会话真实历史时间（epoch ms）：历史推导的状态迁移（Discovered/Closed）与首扫期的
    /// Activity/Waiting/TurnFailed 都带它当 ts，让启动回填被 lib.rs 的 APP_START 闸拦住，
    /// 否则首扫把历史状态伪装成实时变化（ts=now）弹通知风暴。
    time_updated: u64,
    presence: bool,
    latest_turn: Option<TurnRow>,
}
#[derive(Clone)]
struct TurnRow {
    status: String,
    error_type: Option<String>,
    completed_at: Option<i64>,
}

#[derive(Clone, PartialEq)]
enum ZStatus {
    IdleWaiting,
    Running,
    Failed,
    Closed,
}

pub struct ZcodePoller {
    con: rusqlite::Connection,
    known: std::collections::HashSet<String>,
    prev: HashMap<String, ZStatus>,
    /// 首扫（第一次 poll）在回放历史状态而非实时变化：期间 Activity/Waiting/TurnFailed
    /// 的 ts 用会话 time_updated（历史时间 → 被 APP_START 闸静默），之后才用 now。
    first_scan: bool,
    /// presence 会话的 time_updated 距 poll 时刻小于该值判"运行中"。实测 ZCode 的
    /// turn_usage 行只在回合结束（completed/error/cancelled）写入，生成期间库里没有
    /// completed_at IS NULL 的行，但 session.time_updated 每 2s 刷新——新鲜度是当前
    /// ZCode 唯一可靠的运行信号。每次 poll 前由 lib.rs 按 config 注入（set_running_fresh_ms）。
    running_fresh_ms: u64,
}

impl ZcodePoller {
    pub fn open(db_path: &std::path::Path) -> rusqlite::Result<Self> {
        let uri = format!(
            "file:{}?mode=ro",
            db_path.to_string_lossy().replace('\\', "/")
        );
        let con = rusqlite::Connection::open_with_flags(
            &uri,
            rusqlite::OpenFlags::SQLITE_OPEN_URI | rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        Self::with_connection(con)
    }

    pub fn with_connection(con: rusqlite::Connection) -> rusqlite::Result<Self> {
        Ok(Self {
            con,
            known: Default::default(),
            prev: Default::default(),
            first_scan: true,
            running_fresh_ms: 120_000,
        })
    }

    /// 每次 poll 前由 lib.rs 注入 config.running_fresh_ms，设置页改动实时生效
    pub fn set_running_fresh_ms(&mut self, ms: u64) {
        self.running_fresh_ms = ms;
    }

    pub fn poll(&mut self, now: u64) -> Vec<UnifiedEvent> {
        let mut out = Vec::new();
        let mut rows: HashMap<String, Row> = HashMap::new();

        let Ok(mut stmt) = self.con.prepare(
            "SELECT id, title, directory, time_updated FROM session WHERE time_deleted IS NULL AND time_archived IS NULL"
        ) else { return out };
        let _ = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    // 真实库 NOT NULL；防御 NULL/负值退 0（必被启动闸拦截，安全侧）
                    r.get::<_, Option<i64>>(3)?.unwrap_or_default().max(0) as u64,
                ))
            })
            .map(|it| {
                for (id, title, directory, time_updated) in it.flatten() {
                    rows.insert(
                        id,
                        Row {
                            title,
                            directory,
                            time_updated,
                            presence: false,
                            latest_turn: None,
                        },
                    );
                }
            });

        let _ = self
            .con
            .prepare("SELECT session_id FROM session_runtime_presence")
            .map(|mut st| {
                let _ = st.query_map([], |r| r.get::<_, String>(0)).map(|it| {
                    for sid in it.flatten() {
                        if let Some(row) = rows.get_mut(&sid) {
                            row.presence = true;
                        }
                    }
                });
            });

        // 对存活会话（数量小）逐个取最新 turn——按需取数，正确性与全局 turn 数据量解耦
        if let Ok(mut stmt) = self.con.prepare(
            "SELECT status, error_type, completed_at FROM turn_usage WHERE session_id = ?1 ORDER BY started_at DESC LIMIT 1"
        ) {
            let live_ids: Vec<String> = rows.iter().filter(|(_, r)| r.presence).map(|(id, _)| id.clone()).collect();
            for sid in live_ids {
                let _ = stmt.query_row([&sid], |r| {
                    Ok(TurnRow {
                        status: r.get(0)?,
                        error_type: r.get(1)?,
                        completed_at: r.get(2)?,
                    })
                }).map(|t| {
                    if let Some(row) = rows.get_mut(&sid) {
                        row.latest_turn = Some(t);
                    }
                });
            }
        }

        let fresh_ms = self.running_fresh_ms;
        for (id, row) in rows {
            // Running = presence && (进行中 turn || time_updated 新鲜)。实测 ZCode 的
            // turn_usage 只在回合结束写行（completed/error/cancelled），生成期间库里没有
            // completed_at IS NULL 的行 → 仅凭 turn 臂活会话永远显示"空闲"、"运行中→空闲"
            // 边沿与"在等你处理"通知永不出现；而生成中 session.time_updated 每 2s 刷新，
            // 新鲜度是当前 ZCode 唯一可靠的运行信号（turn 臂保留，兼容未来 ZCode 行为）。
            let fresh = row.time_updated.saturating_add(fresh_ms) > now;
            let z = if row.presence {
                match &row.latest_turn {
                    Some(t) if t.completed_at.is_none() || fresh => ZStatus::Running,
                    // 无 turn 行 + time_updated 新鲜 → 生成中的实际库内形态
                    _ if fresh => ZStatus::Running,
                    Some(t)
                        if t.status == "error"
                            && t.error_type.as_deref() != Some("turn_cancelled") =>
                    {
                        ZStatus::Failed
                    }
                    // presence 存活 + time_updated 陈旧 + turn 全完成 → 等待·空闲（进"需处理"组）。
                    // 教训：曾在这里按 last_outcome 判 Closed，而 ZCode 每轮结束都写
                    // last_outcome='completed' → 活会话几乎永远被误标"完成"，
                    // 等待/运行/失败永不出现。只有 presence 消失才算 Closed。
                    _ => ZStatus::IdleWaiting,
                }
            } else {
                ZStatus::Closed
            };

            let agent = AgentKind::ZCode;
            if !self.known.contains(&id) {
                self.known.insert(id.clone());
                // 首扫发现的历史会话：ts 带会话 time_updated（历史推导，不触发通知语义）
                out.push(UnifiedEvent::Discovered {
                    agent,
                    session_id: id.clone(),
                    cwd: row.directory.clone(),
                    title: Some(row.title.clone()),
                    ts: row.time_updated,
                });
            }
            // Activity/Waiting/TurnFailed 是 presence 存活会话的信号：首扫是历史状态回放，
            // ts 用会话 time_updated（被 APP_START 闸静默）；之后才是实时变化，ts=now 照常通知。
            // Closed（presence 消失，历史推导）始终用会话 time_updated，见上方 Row 注释。
            // 首扫发现"存活且进行中"的会话走 Discovered(ts=历史) + Activity(ts=历史) →
            // machine 置 Running、last_activity=历史 → 首个 Tick 立即推断 Done（静默）；
            // 而真正的实时运行（非首扫期）Activity ts=now，阈值内不会被 Tick 误推断。
            let live_ts = if self.first_scan {
                row.time_updated
            } else {
                now
            };
            let prev = self.prev.get(&id);
            match (&z, prev) {
                (ZStatus::Running, Some(ZStatus::Running)) => {}
                (ZStatus::Running, _) => out.push(UnifiedEvent::Activity {
                    agent,
                    session_id: id.clone(),
                    ts: live_ts,
                }),
                (ZStatus::IdleWaiting, Some(ZStatus::IdleWaiting)) => {}
                (ZStatus::IdleWaiting, _) => out.push(UnifiedEvent::Waiting {
                    agent,
                    session_id: id.clone(),
                    kind: WaitingKind::Idle,
                    ts: live_ts,
                }),
                (ZStatus::Failed, Some(ZStatus::Failed)) => {}
                (ZStatus::Failed, _) => out.push(UnifiedEvent::TurnFailed {
                    agent,
                    session_id: id.clone(),
                    ts: live_ts,
                }),
                (ZStatus::Closed, Some(ZStatus::Closed)) => {}
                // 历史推导的关闭（presence 消失）：ts 带会话 time_updated（真实历史时间）
                // → 启动首扫被 APP_START 闸静默，不再通知风暴
                (ZStatus::Closed, _) => out.push(UnifiedEvent::Closed {
                    agent,
                    session_id: id.clone(),
                    ts: row.time_updated,
                }),
            }
            self.prev.insert(id, z);
        }
        self.first_scan = false; // 首扫结束，之后的信号才是实时变化
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn fixture_db() -> Connection {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(r#"
            CREATE TABLE session (id TEXT PRIMARY KEY, title TEXT, directory TEXT,
                time_created INTEGER, time_updated INTEGER, last_outcome TEXT,
                time_deleted INTEGER, time_archived INTEGER);
            CREATE TABLE session_runtime_presence (session_id TEXT, owner_id TEXT, process_id INTEGER);
            CREATE TABLE turn_usage (id INTEGER PRIMARY KEY, session_id TEXT, status TEXT,
                error_type TEXT, started_at INTEGER, completed_at INTEGER);
        "#).unwrap();
        con
    }

    fn insert_session(con: &Connection, id: &str, last_outcome: Option<&str>, time_updated: i64) {
        con.execute("INSERT INTO session (id,title,directory,time_created,time_updated,last_outcome) VALUES (?1,?2,?3,1,?4,?5)",
            rusqlite::params![id, "标题", r"C:\repo\z", time_updated, last_outcome]).unwrap();
    }

    #[test]
    fn discover_then_idle_when_presence_without_active_turn() {
        let con = fixture_db();
        insert_session(&con, "z1", None, 50);
        con.execute(
            "INSERT INTO session_runtime_presence VALUES ('z1','o',1)",
            [],
        )
        .unwrap();
        let mut p = ZcodePoller::with_connection(con).unwrap();
        // poll 时刻取 time_updated=50 的 fresh 窗口（默认 120s）之外：陈旧 + 无 turn → 空闲
        let ev = p.poll(200_000);
        assert!(ev.iter().any(|e| matches!(e, UnifiedEvent::Discovered { session_id, title, .. } if session_id=="z1" && title.as_deref()==Some("标题"))));
        // presence + time_updated 陈旧 + 无进行中 turn → 等待·空闲
        assert!(ev.iter().any(|e| matches!(
            e,
            UnifiedEvent::Waiting {
                kind: WaitingKind::Idle,
                ..
            }
        )));
        // 状态不变 → 第二次无事件（边沿触发）
        assert!(p.poll(300_000).is_empty());
    }

    #[test]
    fn active_turn_is_running_activity() {
        let con = fixture_db();
        insert_session(&con, "z1", None, 50);
        con.execute(
            "INSERT INTO session_runtime_presence VALUES ('z1','o',1)",
            [],
        )
        .unwrap();
        con.execute(
            "INSERT INTO turn_usage (session_id,status,started_at) VALUES ('z1','completed',5)",
            [],
        )
        .unwrap();
        con.execute(
            "INSERT INTO turn_usage (session_id,status,started_at) VALUES ('z1','completed',10)",
            [],
        )
        .unwrap();
        con.execute("INSERT INTO turn_usage (session_id,status,error_type,started_at,completed_at) VALUES ('z1','completed',NULL,20,NULL)", []).unwrap();
        let mut p = ZcodePoller::with_connection(con).unwrap();
        let ev = p.poll(100);
        assert!(
            ev.iter().any(
                |e| matches!(e, UnifiedEvent::Activity { session_id, .. } if session_id=="z1")
            )
        );
    }

    #[test]
    fn error_turn_failed_but_cancelled_not_failed() {
        let con = fixture_db();
        insert_session(&con, "z1", None, 50);
        con.execute(
            "INSERT INTO session_runtime_presence VALUES ('z1','o',1)",
            [],
        )
        .unwrap();
        con.execute("INSERT INTO turn_usage (session_id,status,error_type,started_at,completed_at) VALUES ('z1','error','unknown_error',20,30)", []).unwrap();
        let mut p = ZcodePoller::with_connection(con).unwrap();
        // poll 时刻取 time_updated=50 的 fresh 窗口之外：error 非 cancelled → TurnFailed
        let ev = p.poll(200_000);
        assert!(
            ev.iter()
                .any(|e| matches!(e, UnifiedEvent::TurnFailed { .. }))
        );

        let con2 = fixture_db();
        insert_session(&con2, "z2", None, 50);
        con2.execute(
            "INSERT INTO session_runtime_presence VALUES ('z2','o',1)",
            [],
        )
        .unwrap();
        con2.execute("INSERT INTO turn_usage (session_id,status,error_type,started_at,completed_at) VALUES ('z2','error','turn_cancelled',20,30)", []).unwrap();
        let mut p2 = ZcodePoller::with_connection(con2).unwrap();
        let ev2 = p2.poll(200_000);
        assert!(
            !ev2.iter()
                .any(|e| matches!(e, UnifiedEvent::TurnFailed { .. })),
            "用户打断不算失败"
        );
    }

    #[test]
    fn presence_gone_closes() {
        let con2 = fixture_db();
        insert_session(&con2, "z2", None, 50);
        let mut p2 = ZcodePoller::with_connection(con2).unwrap();
        let ev2 = p2.poll(100);
        assert!(
            ev2.iter()
                .any(|e| matches!(e, UnifiedEvent::Closed { session_id, ts, .. } if session_id=="z2" && *ts == 50)),
            "无 presence 即关闭，ts 是会话历史时间而非轮询时刻"
        );
    }

    /// 回归（验收缺陷：活会话误判 Done）：presence 存活 + last_outcome=completed +
    /// turn 全完成 → 等待·空闲（进"需处理"组），绝不是 Closed/Done——
    /// ZCode 每轮结束都写 last_outcome='completed'，按它判关会把活会话几乎永远标成完成。
    #[test]
    fn presence_with_completed_outcome_is_idle_waiting_not_closed() {
        let con = fixture_db();
        insert_session(&con, "z1", Some("completed"), 50);
        con.execute(
            "INSERT INTO session_runtime_presence VALUES ('z1','o',1)",
            [],
        )
        .unwrap();
        con.execute(
            "INSERT INTO turn_usage (session_id,status,started_at,completed_at) VALUES ('z1','completed',10,20)",
            [],
        )
        .unwrap();
        let mut p = ZcodePoller::with_connection(con).unwrap();
        // poll 时刻取 time_updated=50 的 fresh 窗口之外：陈旧 + turn 全完成 → 等待·空闲
        let ev = p.poll(200_000);
        assert!(
            ev.iter().any(|e| matches!(
                e,
                UnifiedEvent::Waiting { session_id, kind: WaitingKind::Idle, .. } if session_id=="z1"
            )),
            "presence 存活 + turn 全完成 → 等待·空闲"
        );
        assert!(
            !ev.iter().any(|e| matches!(e, UnifiedEvent::Closed { .. })),
            "presence 存活绝不产 Closed"
        );
    }

    /// 回归（首扫回填语义）：首扫是历史状态回放，Activity/Waiting 的 ts 必须是会话
    /// time_updated（被 APP_START 闸静默）；首扫结束后才是实时变化，ts=now。
    #[test]
    fn first_scan_uses_historical_ts_then_realtime_now() {
        // 两轮 poll 之间要改库，用临时文件库（poller 只读打开，改动走另一连接）
        let dir = std::env::temp_dir().join(format!("ab-zc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("scan.sqlite");
        let con = Connection::open(&db).unwrap();
        con.execute_batch(
            r#"CREATE TABLE session (id TEXT PRIMARY KEY, title TEXT, directory TEXT,
                time_created INTEGER, time_updated INTEGER, last_outcome TEXT,
                time_deleted INTEGER, time_archived INTEGER);
            CREATE TABLE session_runtime_presence (session_id TEXT, owner_id TEXT, process_id INTEGER);
            CREATE TABLE turn_usage (id INTEGER PRIMARY KEY, session_id TEXT, status TEXT,
                error_type TEXT, started_at INTEGER, completed_at INTEGER);"#,
        )
        .unwrap();
        insert_session(&con, "z1", None, 50);
        con.execute(
            "INSERT INTO session_runtime_presence VALUES ('z1','o',1)",
            [],
        )
        .unwrap();
        drop(con);
        let mut p = ZcodePoller::open(&db).unwrap();
        // 首扫：turn 全完成 → Waiting，ts = time_updated=50，不是轮询时刻 999_000
        let ev = p.poll(999_000);
        assert!(ev.iter().any(|e| matches!(
            e,
            UnifiedEvent::Waiting { ts, .. } if *ts == 50
        )));
        assert!(
            !ev.iter().any(|e| matches!(
                e,
                UnifiedEvent::Waiting { ts, .. } if *ts == 999_000
            )),
            "首扫 Waiting 不得伪装成实时变化（ts=now）"
        );

        // 首扫后 turn 变为进行中 → 状态变化产 Activity，ts = now（实时信号）
        let con2 = Connection::open(&db).unwrap();
        con2.execute(
            "INSERT INTO turn_usage (session_id,status,started_at,completed_at) VALUES ('z1','completed',900,NULL)",
            [],
        )
        .unwrap();
        drop(con2);
        let ev2 = p.poll(999_000_000);
        assert!(ev2.iter().any(|e| matches!(
            e,
            UnifiedEvent::Activity { ts, .. } if *ts == 999_000_000
        )));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 回归（验收缺陷：启动通知风暴）：无 presence 的历史会话首扫判 Closed 时，
    /// ts 必须是会话的 time_updated（真实历史时间），不得等于轮询时刻 now——
    /// 否则 last_activity=now 绕过 lib.rs 的 APP_START 回填闸，重启即弹一轮"任务完成"。
    #[test]
    fn historical_session_without_presence_closes_with_historical_ts() {
        let con = fixture_db();
        let old_updated = 1_000i64; // 明显早于轮询时刻
        insert_session(&con, "z-hist", None, old_updated);
        let mut p = ZcodePoller::with_connection(con).unwrap();
        let ev = p.poll(999_000_000);
        assert!(
            ev.iter().any(|e| matches!(
                e,
                UnifiedEvent::Discovered { session_id, ts, .. }
                    if session_id == "z-hist" && *ts == old_updated as u64
            )),
            "Discovered 同样携带 time_updated（保持一致性）"
        );
        assert!(
            ev.iter().any(|e| matches!(
                e,
                UnifiedEvent::Closed { session_id, ts, .. }
                    if session_id == "z-hist" && *ts == old_updated as u64
            )),
            "Closed 携带 time_updated 而非 now"
        );
    }

    #[test]
    fn stale_running_turn_survives_global_turn_volume() {
        let con = fixture_db();
        insert_session(&con, "z1", None, 50);
        con.execute(
            "INSERT INTO session_runtime_presence VALUES ('z1','o',1)",
            [],
        )
        .unwrap();
        // z1 的运行中 turn：started_at 很旧、未完成
        con.execute(
            "INSERT INTO turn_usage (session_id,status,started_at) VALUES ('z1','completed',1)",
            [],
        )
        .unwrap();
        // 另一会话插入 600 条更新的 turn：旧的全局 LIMIT 500 窗口会把 z1 挤出 → 误判 Idle
        for i in 2..=601 {
            con.execute("INSERT INTO turn_usage (session_id,status,started_at) VALUES ('z2','completed',?1)", [i]).unwrap();
        }
        let mut p = ZcodePoller::with_connection(con).unwrap();
        let ev = p.poll(100);
        assert!(
            ev.iter().any(
                |e| matches!(e, UnifiedEvent::Activity { session_id, .. } if session_id=="z1")
            ),
            "运行中 turn 不被其他会话的 turn 量挤出窗口"
        );
    }

    /// 回归（v0.1.1：ZCode 运行中判定改用 time_updated 新鲜度）：ZCode 的 turn_usage
    /// 行只在回合结束写，生成期间库里没有 completed_at IS NULL 的行，但 session.time_updated
    /// 每 2s 刷新——presence + time_updated 新鲜必须判 Running，否则活会话全程显示
    /// "空闲"、"运行中→空闲"边沿与"在等你处理"通知永不出现。
    #[test]
    fn fresh_time_updated_means_running_even_when_turn_finished() {
        // ① time_updated 距 poll 时刻 30s（fresh=120s）+ turn 已完结 → Activity（Running）
        let con = fixture_db();
        insert_session(&con, "z1", Some("completed"), 30);
        con.execute(
            "INSERT INTO session_runtime_presence VALUES ('z1','o',1)",
            [],
        )
        .unwrap();
        con.execute(
            "INSERT INTO turn_usage (session_id,status,started_at,completed_at) VALUES ('z1','completed',1,25)",
            [],
        )
        .unwrap();
        let mut p = ZcodePoller::with_connection(con).unwrap();
        p.set_running_fresh_ms(120_000);
        let ev = p.poll(100_000); // 30 + 120_000 > 100_000 → 新鲜
        assert!(
            ev.iter().any(|e| matches!(
                e,
                UnifiedEvent::Activity { session_id, .. } if session_id=="z1"
            )),
            "presence + time_updated 新鲜 + turn 已完结 → Activity（Running）"
        );

        // ② time_updated 距 poll 时刻 10 分钟 + turn 完结 → Waiting(Idle)
        let con = fixture_db();
        insert_session(&con, "z2", Some("completed"), 0);
        con.execute(
            "INSERT INTO session_runtime_presence VALUES ('z2','o',1)",
            [],
        )
        .unwrap();
        con.execute(
            "INSERT INTO turn_usage (session_id,status,started_at,completed_at) VALUES ('z2','completed',1,500)",
            [],
        )
        .unwrap();
        let mut p = ZcodePoller::with_connection(con).unwrap();
        p.set_running_fresh_ms(120_000);
        let ev = p.poll(600_000); // 0 + 120_000 < 600_000 → 陈旧
        assert!(
            ev.iter().any(|e| matches!(
                e,
                UnifiedEvent::Waiting { session_id, kind: WaitingKind::Idle, .. } if session_id=="z2"
            )),
            "time_updated 陈旧 + turn 完结 → 等待·空闲"
        );

        // ③ time_updated 新鲜 + 无 turn 行 → Activity（生成期间的库内实际形态）
        let con = fixture_db();
        insert_session(&con, "z3", None, 99_000);
        con.execute(
            "INSERT INTO session_runtime_presence VALUES ('z3','o',1)",
            [],
        )
        .unwrap();
        let mut p = ZcodePoller::with_connection(con).unwrap();
        let ev = p.poll(100_000); // 默认 fresh=120s：99_000 + 120_000 > 100_000 → 新鲜
        assert!(
            ev.iter().any(|e| matches!(
                e,
                UnifiedEvent::Activity { session_id, .. } if session_id=="z3"
            )),
            "无 turn 行 + time_updated 新鲜 → Running（当前 ZCode 生成中的唯一信号）"
        );

        // ④ 进行中 turn + time_updated 陈旧 → Activity（OR 的 turn 臂，兼容未来 ZCode 行为）
        let con = fixture_db();
        insert_session(&con, "z4", None, 0);
        con.execute(
            "INSERT INTO session_runtime_presence VALUES ('z4','o',1)",
            [],
        )
        .unwrap();
        con.execute(
            "INSERT INTO turn_usage (session_id,status,started_at,completed_at) VALUES ('z4','completed',1,NULL)",
            [],
        )
        .unwrap();
        let mut p = ZcodePoller::with_connection(con).unwrap();
        let ev = p.poll(600_000);
        assert!(
            ev.iter().any(|e| matches!(
                e,
                UnifiedEvent::Activity { session_id, .. } if session_id=="z4"
            )),
            "进行中 turn（completed_at IS NULL）即使 time_updated 陈旧也判 Running"
        );

        // ⑤ set_running_fresh_ms 生效：fresh=1s 时 30s 前的时间戳已陈旧 → Waiting
        let con = fixture_db();
        insert_session(&con, "z5", Some("completed"), 70_000);
        con.execute(
            "INSERT INTO session_runtime_presence VALUES ('z5','o',1)",
            [],
        )
        .unwrap();
        con.execute(
            "INSERT INTO turn_usage (session_id,status,started_at,completed_at) VALUES ('z5','completed',1,71_000)",
            [],
        )
        .unwrap();
        let mut p = ZcodePoller::with_connection(con).unwrap();
        p.set_running_fresh_ms(1_000);
        let ev = p.poll(100_000); // 70_000 + 1_000 < 100_000 → 陈旧
        assert!(
            ev.iter().any(|e| matches!(
                e,
                UnifiedEvent::Waiting { session_id, kind: WaitingKind::Idle, .. } if session_id=="z5"
            )),
            "running_fresh_ms 注入后阈值实时生效"
        );
    }
}
