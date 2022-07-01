use anyhow::Result;
use deadpool_postgres::{Config, ManagerConfig, Pool, RecyclingMethod, Runtime};
use snarkvm::dpc::testnet2::Testnet2;
use snarkvm::dpc::{AleoAmount, Network};
use std::collections::HashMap;
use std::env;
use tokio_postgres::NoTls;
use tracing::warn;

pub struct DB {
    connection_pool: Pool,
}

impl DB {
    pub fn init() -> DB {
        let mut cfg = Config::new();
        cfg.host = Some(env::var("DB_HOST").unwrap_or_else(|_| panic!("No database host defined")));
        cfg.port = Some(
            env::var("DB_PORT")
                .unwrap_or_else(|_| "5432".to_string())
                .parse::<u16>()
                .unwrap_or_else(|_| panic!("Invalid database port")),
        );
        cfg.dbname = Some(env::var("DB_DATABASE").unwrap_or_else(|_| panic!("No database name defined")));
        cfg.user = Some(env::var("DB_USER").unwrap_or_else(|_| panic!("No database user defined")));
        cfg.password = Some(env::var("DB_PASSWORD").unwrap_or_else(|_| panic!("No database password defined")));
        let schema = env::var("DB_SCHEMA").unwrap_or_else(|_| {
            warn!("Using schema public as default");
            "public".to_string()
        });
        cfg.manager = Some(ManagerConfig {
            recycling_method: RecyclingMethod::Custom(format!("set search_path = {}", schema)),
        });
        let pool = cfg
            .create_pool(Some(Runtime::Tokio1), NoTls)
            .unwrap_or_else(|_| panic!("Failed to create database connection pool"));
        DB { connection_pool: pool }
    }

    pub async fn save_block(
        &self,
        height: u32,
        block_hash: <Testnet2 as Network>::BlockHash,
        reward: AleoAmount,
        shares: HashMap<String, u64>,
    ) -> Result<()> {
        let mut conn = self.connection_pool.get().await?;
        let transaction = conn.transaction().await?;

        let block_id: i32 = transaction
            .query_one(
                "INSERT INTO block (height, block_hash, reward) VALUES ($1, $2, $3) RETURNING id",
                &[&(height as i64), &block_hash.to_string(), &reward.as_i64()],
            )
            .await?
            .try_get("id")?;

        let stmt = transaction
            .prepare("INSERT INTO share (block_id, miner, share) VALUES ($1, $2, $3)")
            .await?;
        for (address, share) in shares {
            transaction
                .query(&stmt, &[&block_id, &address, &(share as i64)])
                .await?;
        }

        transaction.commit().await?;
        Ok(())
    }

    pub async fn get_blocks(&self, limit: u16, page: u16) -> Result<Vec<(i32, u32, String, bool, i64)>> {
        let conn = self.connection_pool.get().await?;

        let row = conn.query("SELECT id FROM block ORDER BY id DESC LIMIT 1", &[]).await?;
        if row.is_empty() {
            return Ok(vec![]);
        }
        let last_id: i32 = row.first().unwrap().get("id");
        let stmt = conn.prepare("SELECT * FROM block WHERE id <= $1 AND id > $2").await?;
        let rows = conn
            .query(&stmt, &[&last_id, &(last_id - page as i32 * limit as i32)])
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let id: i32 = row.get("id");
                let height: u32 = row.get("height");
                let block_hash: String = row.get("block_hash");
                let reward: i64 = row.get("reward");
                let is_canonical: bool = row.get("is_canonical");
                (id, height, block_hash, is_canonical, reward)
            })
            .collect())
    }

    pub async fn set_block_canonical(&self, block_hash: String, is_canonical: bool) -> Result<()> {
        let mut conn = self.connection_pool.get().await?;
        let transaction = conn.transaction().await?;
        let stmt = transaction
            .prepare("UPDATE block SET is_canonical = $1 WHERE block_hash = $2")
            .await?;
        transaction.query(&stmt, &[&is_canonical, &block_hash]).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn get_block_shares(&self, id: Vec<i32>) -> Result<HashMap<i32, HashMap<String, i64>>> {
        let conn = self.connection_pool.get().await?;
        let stmt = conn.prepare("SELECT * FROM share WHERE block_id = ANY($1)").await?;

        let res = conn.query(&stmt, &[&id]).await?;
        let mut shares = HashMap::new();
        for row in res {
            let block_id: i32 = row.get("block_id");
            let miner: String = row.get("miner");
            let share: i64 = row.get("share");
            shares.entry(block_id).or_insert_with(HashMap::new).insert(miner, share);
        }
        Ok(shares)
    }
}
