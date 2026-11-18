use crate::core::types::*;

pub struct History {
    con: rusqlite::Connection,
}

impl History {
    pub fn open(path: &std::path::Path) -> rusqlite::Result<Self> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).ok();
        }
        let con = rusqlite::Connection::open(path)?;
        Self::with_connection(con)
    }

    pub fn with_connection(con: rusqlite::Connection) -> rusqlite::Result<Self> {
        con.execute_batch(Self::INIT_SCHEMA)?;
        // v0.1.1 旧库升级：缺列则补（已存在时报 duplicate column，忽略即迁移完成）
        let _ = con.execute("ALTER TABLE sessions ADD COLUMN waiting_reason TEXT", []);
        let _ = con.execute("ALTER TABLE sessions ADD COLUMN reply_head TEXT", []);
        Ok(Self { con })
    }

    const INIT_SCHEMA: &str = r#"
        CREATE TABLE IF NOT EXISTS sessions (
            session_id TEXT PRIMARY KEY, agent TEXT, title TEXT, cwd TEXT, repo TEXT,
            branch TEXT, status TEXT, done_inferred INTEGER,
            waiting_reason TEXT, reply_head TEXT,
            last_activity INTEGER, time_created INTEGER
        );
    "#;

    pub fn upsert(&self, s: &SessionSnapshot) -> rusqlite::Result<()> {
        // 显式列名：迁移库（ALTER 补列在末尾）与新库（建表即含两列）物理列序不同，
        // 不带列名的 VALUES 会错位写入
        self.con.execute(
            "INSERT INTO sessions
             (session_id, agent, title, cwd, repo, branch, status, done_inferred, waiting_reason, reply_head, last_activity, time_created)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
             ON CONFLICT(session_id) DO UPDATE SET title=?3, status=?7, done_inferred=?8, waiting_reason=?9, reply_head=?10, last_activity=?11",
            rusqlite::params![
                s.session_id, serde_json::to_string(&s.agent).unwrap(),
                s.title, s.cwd, s.repo,
                s.branch, serde_json::to_string(&s.status).unwrap(),
                s.done_inferred as i64, s.waiting_reason, s.reply_head,
                s.last_activity as i64, s.time_created as i64,
            ])?;
        Ok(())
    }

    pub fn list(&self) -> rusqlite::Result<Vec<SessionSnapshot>> {
        let mut stmt = self.con.prepare("SELECT session_id, agent, title, cwd, repo, branch, status, done_inferred, waiting_reason, reply_head, last_activity, time_created FROM sessions ORDER BY last_activity DESC")?;
        let rows = stmt.query_map([], |r| {
            Ok(SessionSnapshot {
                session_id: r.get(0)?,
                agent: serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or(AgentKind::ZCode),
                title: r.get(2)?,
                cwd: r.get(3)?,
                repo: r.get(4)?,
                branch: r.get(5)?,
                status: serde_json::from_str(&r.get::<_, String>(6)?)
                    .unwrap_or(SessionStatus::Done),
                done_inferred: r.get::<_, i64>(7)? != 0,
                waiting_reason: r.get(8)?,
                reply_head: r.get(9)?,
                last_activity: r.get::<_, i64>(10)? as u64,
                time_created: r.get::<_, i64>(11)? as u64,
            })
        })?;
        Ok(rows.flatten().collect())
    }

    pub fn prune(&self, keep: usize) -> rusqlite::Result<()> {
        self.con.execute(
            "DELETE FROM sessions WHERE session_id NOT IN (
                SELECT session_id FROM sessions ORDER BY last_activity DESC LIMIT ?1)",
            (keep as i64,),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(id: &str, activity: u64) -> SessionSnapshot {
        SessionSnapshot {
            session_id: id.into(),
            agent: AgentKind::ZCode,
            title: "t".into(),
            cwd: r"C:\repo\z".into(),
            repo: "z".into(),
            branch: None,
            status: SessionStatus::Done,
            done_inferred: false,
            waiting_reason: None,
            reply_head: None,
            last_activity: activity,
            time_created: activity,
        }
    }

    #[test]
    fn upsert_list_and_prune() {
        let con = rusqlite::Connection::open_in_memory().unwrap();
        let h = History::with_connection(con).unwrap();
        h.upsert(&snap("a", 1)).unwrap();
        h.upsert(&snap("b", 2)).unwrap();
        h.upsert(&snap("a", 9)).unwrap(); // 覆盖
        let list = h.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].session_id, "a");
        assert_eq!(list[0].last_activity, 9);

        for i in 0..1100u64 {
            h.upsert(&snap(&format!("x{i}"), 100 + i)).unwrap();
        }
        h.prune(1000).unwrap();
        assert_eq!(h.list().unwrap().len(), 1000);
    }

    // v0.1.3：waiting_reason / reply_head 读写 roundtrip（覆盖时更新、None 清空）
    #[test]
    fn waiting_reason_and_reply_head_roundtrip() {
        let con = rusqlite::Connection::open_in_memory().unwrap();
        let h = History::with_connection(con).unwrap();
        let mut s = snap("a", 1);
        s.status = SessionStatus::Waiting(WaitingKind::Permission);
        s.waiting_reason = Some("git push origin main".into());
        h.upsert(&s).unwrap();
        let list = h.list().unwrap();
        assert_eq!(
            list[0].waiting_reason.as_deref(),
            Some("git push origin main")
        );
        assert_eq!(list[0].reply_head, None);

        // 覆盖：换成完成摘要
        let mut s2 = snap("a", 9);
        s2.reply_head = Some("构建完成并推送".into());
        s2.waiting_reason = None;
        h.upsert(&s2).unwrap();
        let list = h.list().unwrap();
        assert_eq!(list[0].reply_head.as_deref(), Some("构建完成并推送"));
        assert_eq!(list[0].waiting_reason, None);
    }

    // v0.1.1 旧库（无新列）打开即迁移：ALTER 补列，旧行可读、新行可写
    #[test]
    fn migrates_legacy_schema_without_new_columns() {
        let con = rusqlite::Connection::open_in_memory().unwrap();
        con.execute_batch(
            "CREATE TABLE sessions (
                session_id TEXT PRIMARY KEY, agent TEXT, title TEXT, cwd TEXT, repo TEXT,
                branch TEXT, status TEXT, done_inferred INTEGER,
                last_activity INTEGER, time_created INTEGER
            );",
        )
        .unwrap();
        con.execute(
            "INSERT INTO sessions VALUES ('old',?1,'t',?2,'z',NULL,?3,0,1,1)",
            rusqlite::params![
                serde_json::to_string(&AgentKind::ZCode).unwrap(),
                r"C:\repo\z",
                serde_json::to_string(&SessionStatus::Done).unwrap(),
            ],
        )
        .unwrap();
        let h = History::with_connection(con).unwrap();
        let list = h.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].session_id, "old");
        assert_eq!(list[0].waiting_reason, None);
        assert_eq!(list[0].reply_head, None);
        h.upsert(&snap("new", 2)).unwrap();
        assert_eq!(h.list().unwrap().len(), 2);
    }
}
