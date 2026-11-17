use crate::core::types::*;

pub struct History { con: rusqlite::Connection }

impl History {
    pub fn open(path: &std::path::Path) -> rusqlite::Result<Self> {
        if let Some(p) = path.parent() { std::fs::create_dir_all(p).ok(); }
        let con = rusqlite::Connection::open(path)?;
        Self::with_connection(con)
    }

    pub fn with_connection(con: rusqlite::Connection) -> rusqlite::Result<Self> {
        con.execute_batch(Self::INIT_SCHEMA)?;
        Ok(Self { con })
    }

    const INIT_SCHEMA: &str = r#"
        CREATE TABLE IF NOT EXISTS sessions (
            session_id TEXT PRIMARY KEY, agent TEXT, title TEXT, cwd TEXT, repo TEXT,
            branch TEXT, status TEXT, done_inferred INTEGER,
            last_activity INTEGER, time_created INTEGER
        );
    "#;

    pub fn upsert(&self, s: &SessionSnapshot) -> rusqlite::Result<()> {
        self.con.execute(
            "INSERT INTO sessions VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
             ON CONFLICT(session_id) DO UPDATE SET title=?3, status=?7, done_inferred=?8, last_activity=?9",
            rusqlite::params![
                s.session_id, serde_json::to_string(&s.agent).unwrap(),
                s.title, s.cwd, s.repo,
                s.branch, serde_json::to_string(&s.status).unwrap(),
                s.done_inferred as i64, s.last_activity as i64, s.time_created as i64,
            ])?;
        Ok(())
    }

    pub fn list(&self) -> rusqlite::Result<Vec<SessionSnapshot>> {
        let mut stmt = self.con.prepare("SELECT session_id, agent, title, cwd, repo, branch, status, done_inferred, last_activity, time_created FROM sessions ORDER BY last_activity DESC")?;
        let rows = stmt.query_map([], |r| {
            Ok(SessionSnapshot {
                session_id: r.get(0)?,
                agent: serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or(AgentKind::ZCode),
                title: r.get(2)?, cwd: r.get(3)?, repo: r.get(4)?, branch: r.get(5)?,
                status: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or(SessionStatus::Done),
                done_inferred: r.get::<_, i64>(7)? != 0,
                last_activity: r.get::<_, i64>(8)? as u64,
                time_created: r.get::<_, i64>(9)? as u64,
            })
        })?;
        Ok(rows.flatten().collect())
    }

    pub fn prune(&self, keep: usize) -> rusqlite::Result<()> {
        self.con.execute(
            "DELETE FROM sessions WHERE session_id NOT IN (
                SELECT session_id FROM sessions ORDER BY last_activity DESC LIMIT ?1)", (keep as i64,))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(id: &str, activity: u64) -> SessionSnapshot {
        SessionSnapshot {
            session_id: id.into(), agent: AgentKind::ZCode, title: "t".into(),
            cwd: r"C:\repo\z".into(), repo: "z".into(), branch: None,
            status: SessionStatus::Done, done_inferred: false,
            last_activity: activity, time_created: activity,
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

        for i in 0..1100u64 { h.upsert(&snap(&format!("x{i}"), 100 + i)).unwrap(); }
        h.prune(1000).unwrap();
        assert_eq!(h.list().unwrap().len(), 1000);
    }
}
