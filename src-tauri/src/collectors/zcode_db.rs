// ZCode db 只读采集器：轮询 ~/.zcode/cli/db/db.sqlite（WAL），把会话/存活/turn 状态
// 转成 UnifiedEvent 边沿事件（状态不变则不产事件）。
use crate::core::types::*;
use std::collections::HashMap;

#[derive(Clone)]
struct Row {
    title: String,
    directory: String,
    last_outcome: Option<String>,
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
        })
    }

    pub fn poll(&mut self, now: u64) -> Vec<UnifiedEvent> {
        let mut out = Vec::new();
        let mut rows: HashMap<String, Row> = HashMap::new();

        let Ok(mut stmt) = self.con.prepare(
            "SELECT id, title, directory, last_outcome FROM session WHERE time_deleted IS NULL AND time_archived IS NULL"
        ) else { return out };
        let _ = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })
            .map(|it| {
                for (id, title, directory, last_outcome) in it.flatten() {
                    rows.insert(
                        id,
                        Row {
                            title,
                            directory,
                            last_outcome,
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

        for (id, row) in rows {
            let z = if row.presence {
                match &row.latest_turn {
                    Some(t) if t.completed_at.is_none() => ZStatus::Running,
                    Some(t)
                        if t.status == "error"
                            && t.error_type.as_deref() != Some("turn_cancelled") =>
                    {
                        ZStatus::Failed
                    }
                    // last_outcome 明确完成 → 已完成（接口："last_outcome IN ('completed','interrupted') 或从 presence 消失 → Closed"）
                    _ if matches!(
                        row.last_outcome.as_deref(),
                        Some("completed") | Some("interrupted")
                    ) =>
                    {
                        ZStatus::Closed
                    }
                    _ => ZStatus::IdleWaiting,
                }
            } else {
                ZStatus::Closed
            };

            let agent = AgentKind::ZCode;
            if !self.known.contains(&id) {
                self.known.insert(id.clone());
                out.push(UnifiedEvent::Discovered {
                    agent,
                    session_id: id.clone(),
                    cwd: row.directory.clone(),
                    title: Some(row.title.clone()),
                    ts: now,
                });
            }
            let prev = self.prev.get(&id);
            match (&z, prev) {
                (ZStatus::Running, Some(ZStatus::Running)) => {}
                (ZStatus::Running, _) => out.push(UnifiedEvent::Activity {
                    agent,
                    session_id: id.clone(),
                    ts: now,
                }),
                (ZStatus::IdleWaiting, Some(ZStatus::IdleWaiting)) => {}
                (ZStatus::IdleWaiting, _) => out.push(UnifiedEvent::Waiting {
                    agent,
                    session_id: id.clone(),
                    kind: WaitingKind::Idle,
                    ts: now,
                }),
                (ZStatus::Failed, Some(ZStatus::Failed)) => {}
                (ZStatus::Failed, _) => out.push(UnifiedEvent::TurnFailed {
                    agent,
                    session_id: id.clone(),
                    ts: now,
                }),
                (ZStatus::Closed, Some(ZStatus::Closed)) => {}
                (ZStatus::Closed, _) => out.push(UnifiedEvent::Closed {
                    agent,
                    session_id: id.clone(),
                    ts: now,
                }),
            }
            self.prev.insert(id, z);
        }
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

    fn insert_session(con: &Connection, id: &str, last_outcome: Option<&str>) {
        con.execute("INSERT INTO session (id,title,directory,time_created,time_updated,last_outcome) VALUES (?1,?2,?3,1,1,?4)",
            rusqlite::params![id, "标题", r"C:\repo\z", last_outcome]).unwrap();
    }

    #[test]
    fn discover_then_idle_when_presence_without_active_turn() {
        let con = fixture_db();
        insert_session(&con, "z1", None);
        con.execute(
            "INSERT INTO session_runtime_presence VALUES ('z1','o',1)",
            [],
        )
        .unwrap();
        let mut p = ZcodePoller::with_connection(con).unwrap();
        let ev = p.poll(100);
        assert!(ev.iter().any(|e| matches!(e, UnifiedEvent::Discovered { session_id, title, .. } if session_id=="z1" && title.as_deref()==Some("标题"))));
        // presence + 无进行中 turn → 等待·空闲
        assert!(ev.iter().any(|e| matches!(
            e,
            UnifiedEvent::Waiting {
                kind: WaitingKind::Idle,
                ..
            }
        )));
        // 状态不变 → 第二次无事件（边沿触发）
        assert!(p.poll(200).is_empty());
    }

    #[test]
    fn active_turn_is_running_activity() {
        let con = fixture_db();
        insert_session(&con, "z1", None);
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
        insert_session(&con, "z1", None);
        con.execute(
            "INSERT INTO session_runtime_presence VALUES ('z1','o',1)",
            [],
        )
        .unwrap();
        con.execute("INSERT INTO turn_usage (session_id,status,error_type,started_at,completed_at) VALUES ('z1','error','unknown_error',20,30)", []).unwrap();
        let mut p = ZcodePoller::with_connection(con).unwrap();
        let ev = p.poll(100);
        assert!(
            ev.iter()
                .any(|e| matches!(e, UnifiedEvent::TurnFailed { .. }))
        );

        let con2 = fixture_db();
        insert_session(&con2, "z2", None);
        con2.execute(
            "INSERT INTO session_runtime_presence VALUES ('z2','o',1)",
            [],
        )
        .unwrap();
        con2.execute("INSERT INTO turn_usage (session_id,status,error_type,started_at,completed_at) VALUES ('z2','error','turn_cancelled',20,30)", []).unwrap();
        let mut p2 = ZcodePoller::with_connection(con2).unwrap();
        let ev2 = p2.poll(100);
        assert!(
            !ev2.iter()
                .any(|e| matches!(e, UnifiedEvent::TurnFailed { .. })),
            "用户打断不算失败"
        );
    }

    #[test]
    fn presence_gone_or_last_outcome_closes() {
        let con = fixture_db();
        insert_session(&con, "z1", Some("completed"));
        con.execute(
            "INSERT INTO session_runtime_presence VALUES ('z1','o',1)",
            [],
        )
        .unwrap();
        let mut p = ZcodePoller::with_connection(con).unwrap();
        let ev = p.poll(100);
        assert!(
            ev.iter()
                .any(|e| matches!(e, UnifiedEvent::Closed { session_id, .. } if session_id=="z1"))
        );

        let con2 = fixture_db();
        insert_session(&con2, "z2", None);
        let mut p2 = ZcodePoller::with_connection(con2).unwrap();
        let ev2 = p2.poll(100);
        assert!(
            ev2.iter()
                .any(|e| matches!(e, UnifiedEvent::Closed { session_id, .. } if session_id=="z2")),
            "无 presence 即关闭"
        );
    }

    #[test]
    fn stale_running_turn_survives_global_turn_volume() {
        let con = fixture_db();
        insert_session(&con, "z1", None);
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
}
