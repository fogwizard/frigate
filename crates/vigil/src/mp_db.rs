//! SQLite tables for miniprogram sessions, boarding, and door challenges.

use anyhow::Result;
use chrono::{Duration, Utc};
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use crate::db::Db;

#[derive(Debug, Clone)]
pub struct MpSession {
    pub openid: String,
    pub role: String,
}

#[derive(Debug, Clone)]
pub struct BoardingRecord {
    pub id: String,
    pub camera: String,
    pub number: String,
    pub label: String,
    pub status: String,
    pub payment_status: String,
}

#[derive(Debug, Clone)]
pub struct CageRow {
    pub id: String,
    pub number: String,
    pub camera: String,
    pub amount: i64,
    pub available: bool,
}

#[derive(Debug, Clone)]
pub struct OrderRow {
    pub id: String,
    pub openid: String,
    pub camera: String,
    pub amount: i64,
    pub status: String,
    pub payment_status: String,
    pub record_id: Option<String>,
}

impl Db {
    pub fn migrate_miniprogram(&self) -> Result<()> {
        let conn = self.lock()?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS mp_sessions (
                token TEXT PRIMARY KEY,
                openid TEXT NOT NULL,
                role TEXT NOT NULL,
                expires_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_mp_sessions_openid ON mp_sessions(openid);

            CREATE TABLE IF NOT EXISTS boarding_stores (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS boarding_qr (
                token TEXT PRIMARY KEY,
                store_id TEXT NOT NULL,
                expires_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS cages (
                id TEXT PRIMARY KEY,
                store_id TEXT NOT NULL,
                number TEXT NOT NULL,
                camera TEXT NOT NULL,
                label TEXT NOT NULL,
                amount_cents INTEGER NOT NULL,
                available INTEGER NOT NULL DEFAULT 1
            );

            CREATE TABLE IF NOT EXISTS boarding_orders (
                id TEXT PRIMARY KEY,
                openid TEXT NOT NULL,
                request_id TEXT NOT NULL,
                store_id TEXT NOT NULL,
                cage_id TEXT NOT NULL,
                camera TEXT NOT NULL,
                number TEXT NOT NULL,
                label TEXT NOT NULL,
                amount_cents INTEGER NOT NULL,
                status TEXT NOT NULL,
                payment_status TEXT NOT NULL,
                record_id TEXT,
                UNIQUE(openid, request_id)
            );

            CREATE TABLE IF NOT EXISTS boarding_records (
                id TEXT PRIMARY KEY,
                openid TEXT NOT NULL,
                order_id TEXT NOT NULL,
                camera TEXT NOT NULL,
                number TEXT NOT NULL,
                label TEXT NOT NULL,
                status TEXT NOT NULL,
                payment_status TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_boarding_records_openid ON boarding_records(openid);

            CREATE TABLE IF NOT EXISTS door_tokens (
                token TEXT PRIMARY KEY,
                store_id TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                consumed INTEGER NOT NULL DEFAULT 0
            );
            ",
        )?;
        Ok(())
    }

    /// Seed demo store/cages/QR when empty (idempotent).
    pub fn seed_miniprogram_demo(&self, camera_ids: &[String]) -> Result<()> {
        let conn = self.lock()?;
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM boarding_stores", [], |r| r.get(0))?;
        if n > 0 {
            return Ok(());
        }
        let store_id = "store_demo";
        conn.execute(
            "INSERT INTO boarding_stores(id, name) VALUES (?1, ?2)",
            params![store_id, "演示宠物店"],
        )?;
        let exp = (Utc::now() + Duration::days(3650)).to_rfc3339();
        conn.execute(
            "INSERT INTO boarding_qr(token, store_id, expires_at) VALUES (?1, ?2, ?3)",
            params!["boarding_token_demo12345", store_id, exp],
        )?;
        conn.execute(
            "INSERT INTO door_tokens(token, store_id, expires_at, consumed) VALUES (?1, ?2, ?3, 0)",
            params!["door_challenge_demo123456", store_id, exp],
        )?;
        for (i, cam) in camera_ids.iter().enumerate() {
            let num = format!("{:02}", i + 1);
            let cage_id = format!("cage_{num}");
            let label = format!("寄养房 {cam}");
            conn.execute(
                "INSERT INTO cages(id, store_id, number, camera, label, amount_cents, available)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)",
                params![cage_id, store_id, num, cam, label, 100], // ¥1.00 demo
            )?;
        }
        tracing::info!(cameras = camera_ids.len(), "seeded miniprogram demo store/cages/QR");
        Ok(())
    }

    pub fn create_session(&self, openid: &str, role: &str, ttl_secs: i64) -> Result<String> {
        let token = Uuid::new_v4().to_string();
        let exp = (Utc::now() + Duration::seconds(ttl_secs)).to_rfc3339();
        self.lock()?.execute(
            "INSERT INTO mp_sessions(token, openid, role, expires_at) VALUES (?1, ?2, ?3, ?4)",
            params![token, openid, role, exp],
        )?;
        Ok(token)
    }

    pub fn get_session(&self, token: &str) -> Result<Option<MpSession>> {
        let conn = self.lock()?;
        let row: Option<(String, String, String)> = conn
            .query_row(
                "SELECT openid, role, expires_at FROM mp_sessions WHERE token = ?1",
                params![token],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((openid, role, exp)) = row else {
            return Ok(None);
        };
        let exp = chrono::DateTime::parse_from_rfc3339(&exp)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or(Utc::now());
        if exp < Utc::now() {
            let _ = conn.execute("DELETE FROM mp_sessions WHERE token = ?1", params![token]);
            return Ok(None);
        }
        Ok(Some(MpSession { openid, role }))
    }

    pub fn list_boarding_records(&self, openid: &str) -> Result<Vec<BoardingRecord>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, camera, number, label, status, payment_status
             FROM boarding_records WHERE openid = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![openid], |r| {
            Ok(BoardingRecord {
                id: r.get(0)?,
                camera: r.get(1)?,
                number: r.get(2)?,
                label: r.get(3)?,
                status: r.get(4)?,
                payment_status: r.get(5)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn has_active_boarding(&self, openid: &str, camera: &str) -> Result<bool> {
        let conn = self.lock()?;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM boarding_records
             WHERE openid = ?1 AND camera = ?2 AND status = 'active' AND payment_status = 'paid'",
            params![openid, camera],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    pub fn resolve_boarding_qr(&self, token: &str) -> Result<Option<(String, String)>> {
        // (store_id, store_name)
        let conn = self.lock()?;
        let row: Option<(String, String, String)> = conn
            .query_row(
                "SELECT q.store_id, s.name, q.expires_at FROM boarding_qr q
                 JOIN boarding_stores s ON s.id = q.store_id WHERE q.token = ?1",
                params![token],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((store_id, name, exp)) = row else {
            return Ok(None);
        };
        let exp = chrono::DateTime::parse_from_rfc3339(&exp)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now() - Duration::hours(1));
        if exp < Utc::now() {
            return Ok(None);
        }
        Ok(Some((store_id, name)))
    }

    pub fn list_cages(&self, store_id: &str) -> Result<Vec<CageRow>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, number, camera, amount_cents, available FROM cages WHERE store_id = ?1",
        )?;
        let rows = stmt.query_map(params![store_id], |r| {
            Ok(CageRow {
                id: r.get(0)?,
                number: r.get(1)?,
                camera: r.get(2)?,
                amount: r.get(3)?,
                available: r.get::<_, i64>(4)? != 0,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn get_cage(&self, cage_id: &str) -> Result<Option<(CageRow, String, String)>> {
        // cage, store_id, label
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, number, camera, amount_cents, available, store_id, label FROM cages WHERE id = ?1",
            params![cage_id],
            |r| {
                Ok((
                    CageRow {
                        id: r.get(0)?,
                        number: r.get(1)?,
                        camera: r.get(2)?,
                        amount: r.get(3)?,
                        available: r.get::<_, i64>(4)? != 0,
                    },
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                ))
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn find_order_by_request(&self, openid: &str, request_id: &str) -> Result<Option<OrderRow>> {
        self.load_order_where("openid = ?1 AND request_id = ?2", params![openid, request_id])
    }

    pub fn get_order(&self, order_id: &str) -> Result<Option<OrderRow>> {
        self.load_order_where("id = ?1", params![order_id])
    }

    fn load_order_where(&self, where_sql: &str, params: impl rusqlite::Params) -> Result<Option<OrderRow>> {
        let conn = self.lock()?;
        let sql = format!(
            "SELECT id, openid, camera, amount_cents, status, payment_status, record_id
             FROM boarding_orders WHERE {where_sql}"
        );
        conn.query_row(&sql, params, |r| {
            Ok(OrderRow {
                id: r.get(0)?,
                openid: r.get(1)?,
                camera: r.get(2)?,
                amount: r.get(3)?,
                status: r.get(4)?,
                payment_status: r.get(5)?,
                record_id: r.get(6)?,
            })
        })
        .optional()
        .map_err(Into::into)
    }

    pub fn create_boarding_order(
        &self,
        openid: &str,
        request_id: &str,
        store_id: &str,
        cage: &CageRow,
        label: &str,
        demo_paid: bool,
    ) -> Result<OrderRow> {
        if let Some(existing) = self.find_order_by_request(openid, request_id)? {
            return Ok(existing);
        }
        let order_id = format!("order_{}", Uuid::new_v4());
        let (status, payment_status, record_id) = if demo_paid {
            let rid = format!("record_{}", Uuid::new_v4());
            ( "active".to_string(), "paid".to_string(), Some(rid) )
        } else {
            ("pending".into(), "pending".into(), None)
        };

        {
            let conn = self.lock()?;
            conn.execute(
                "INSERT INTO boarding_orders(
                    id, openid, request_id, store_id, cage_id, camera, number, label,
                    amount_cents, status, payment_status, record_id
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![
                    order_id,
                    openid,
                    request_id,
                    store_id,
                    cage.id,
                    cage.camera,
                    cage.number,
                    label,
                    cage.amount,
                    status,
                    payment_status,
                    record_id,
                ],
            )?;
            if demo_paid {
                if let Some(ref rid) = record_id {
                    conn.execute(
                        "INSERT INTO boarding_records(
                            id, openid, order_id, camera, number, label, status, payment_status
                         ) VALUES (?1,?2,?3,?4,?5,?6,'active','paid')",
                        params![rid, openid, order_id, cage.camera, cage.number, label],
                    )?;
                    conn.execute(
                        "UPDATE cages SET available = 0 WHERE id = ?1",
                        params![cage.id],
                    )?;
                }
            }
        }
        self.get_order(&order_id)?
            .ok_or_else(|| anyhow::anyhow!("order missing after insert"))
    }

    pub fn consume_door_token(&self, token: &str) -> Result<bool> {
        let conn = self.lock()?;
        let row: Option<(String, i64)> = conn
            .query_row(
                "SELECT expires_at, consumed FROM door_tokens WHERE token = ?1",
                params![token],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((exp, consumed)) = row else {
            return Ok(false);
        };
        if consumed != 0 {
            return Ok(false);
        }
        let exp = chrono::DateTime::parse_from_rfc3339(&exp)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now() - Duration::hours(1));
        if exp < Utc::now() {
            return Ok(false);
        }
        let n = conn.execute(
            "UPDATE door_tokens SET consumed = 1 WHERE token = ?1 AND consumed = 0",
            params![token],
        )?;
        Ok(n == 1)
    }
}
