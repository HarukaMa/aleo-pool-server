use std::{
    collections::{HashMap, VecDeque},
    sync::{atomic::AtomicBool, Arc},
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Error, Result};
use cache::Cache;
use dirs::home_dir;
use parking_lot::RwLock;
use savefile::{load_file, save_file};
use savefile_derive::Savefile;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use snarkvm::prelude::{Network, Testnet3};
use tokio::{
    sync::{
        mpsc::{channel, Sender},
        RwLock as TokioRwLock,
    },
    task,
    time::sleep,
};
use tracing::{debug, error, info};

#[cfg(feature = "db")]
use crate::db::DB;
use crate::{
    accounting::AccountingMessage::NewShare,
    AccountingMessage::{Exit, NewBlock, SetN},
};

trait PayoutModel {
    fn add_share(&mut self, share: Share);
}

#[derive(Clone, Savefile)]
struct Share {
    value: u64,
    owner: String,
}

impl Share {
    pub fn init(value: u64, owner: String) -> Self {
        Share { value, owner }
    }
}

#[allow(clippy::upper_case_acronyms)]
#[derive(Clone, Savefile)]
struct PPLNS {
    queue: VecDeque<Share>,
    current_n: Arc<RwLock<u64>>,
    n: Arc<RwLock<u64>>,
}

impl PPLNS {
    pub fn load() -> Self {
        let home = home_dir();
        if home.is_none() {
            panic!("No home directory found");
        }
        let db_path = home.unwrap().join(".aleo_pool_testnet3_pre2/state");
        match load_file(db_path, 0) {
            Ok(Some(pplns)) => pplns,
            Ok(None) | Err(_) => PPLNS {
                queue: VecDeque::new(),
                current_n: Default::default(),
                n: Default::default(),
            },
        }
    }

    pub fn save(&self) -> std::result::Result<(), Error> {
        let home = home_dir();
        if home.is_none() {
            panic!("No home directory found");
        }
        let db_path = home.unwrap().join(".aleo_pool_testnet3_pre2/state");
        save_file(db_path, 0, self).map_err(|e| anyhow!("Failed to save PPLNS state: {}", e))
    }

    pub fn set_n(&mut self, n: u64) {
        let start = Instant::now();
        let mut current_n = self.current_n.write();
        let mut self_n = self.n.write();
        if n < *self_n {
            while *current_n > n {
                let share = self.queue.pop_front().unwrap();
                *current_n -= share.value;
            }
        }
        *self_n = n;
        debug!("set_n took {} us", start.elapsed().as_micros());
    }
}

impl PayoutModel for PPLNS {
    fn add_share(&mut self, share: Share) {
        let start = Instant::now();
        self.queue.push_back(share.clone());
        let mut current_n = self.current_n.write();
        let self_n = self.n.read();
        *current_n += share.value;
        while *current_n > *self_n {
            let share = self.queue.pop_front().unwrap();
            *current_n -= share.value;
        }
        debug!("add_share took {} us", start.elapsed().as_micros());
        debug!("n: {} / {}", *current_n, self_n);
    }
}

#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash)]
struct Null {}

pub enum AccountingMessage {
    NewShare(String, u64),
    SetN(u64),
    NewBlock(u32, <Testnet3 as Network>::BlockHash, i64),
    Exit,
}

#[cfg(feature = "db")]
static PAY_INTERVAL: Duration = Duration::from_secs(60);

#[allow(clippy::type_complexity)]
pub struct Accounting {
    operator: String,
    pplns: Arc<TokioRwLock<PPLNS>>,
    #[cfg(feature = "db")]
    database: Arc<DB>,
    sender: Sender<AccountingMessage>,
    round_cache: TokioRwLock<Cache<Null, (u32, HashMap<String, u64>)>>,
    block_canonical_cache: TokioRwLock<Cache<String, bool>>,
    exit_lock: Arc<AtomicBool>,
}

impl Accounting {
    pub fn init(operator: String) -> Arc<Accounting> {
        #[cfg(feature = "db")]
        let database = Arc::new(DB::init());

        let pplns = Arc::new(TokioRwLock::new(PPLNS::load()));

        let (sender, mut receiver) = channel(1024);

        let operator_host = operator.split(':').collect::<Vec<&str>>()[0];
        let accounting = Accounting {
            operator: operator_host.into(),
            pplns,
            #[cfg(feature = "db")]
            database,
            sender,
            round_cache: TokioRwLock::new(Cache::new(Duration::from_secs(10))),
            block_canonical_cache: TokioRwLock::new(Cache::new(Duration::from_secs(300))),
            exit_lock: Arc::new(AtomicBool::new(false)),
        };

        let pplns = accounting.pplns.clone();
        #[cfg(feature = "db")]
        let database = accounting.database.clone();
        let exit_lock = accounting.exit_lock.clone();
        task::spawn(async move {
            while let Some(request) = receiver.recv().await {
                match request {
                    NewShare(address, value) => {
                        pplns.write().await.add_share(Share::init(value, address.clone()));
                        debug!("Recorded share from {} with value {}", address, value);
                    }
                    SetN(n) => {
                        pplns.write().await.set_n(n);
                        debug!("Set N to {}", n);
                    }
                    NewBlock(height, block_hash, reward) => {
                        let pplns = pplns.read().await.clone();
                        let (_, address_shares) = Accounting::pplns_to_provers_shares(&pplns);

                        #[cfg(feature = "db")]
                        if let Err(e) = database.save_block(height, block_hash, reward, address_shares).await {
                            error!("Failed to save block reward : {}", e);
                        } else {
                            info!("Recorded block {}", height);
                        }
                    }
                    Exit => {
                        receiver.close();
                        let _ = pplns.read().await.save();
                        exit_lock.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                }
            }
        });

        // backup pplns
        let pplns = accounting.pplns.clone();
        task::spawn(async move {
            loop {
                sleep(Duration::from_secs(60)).await;
                if let Err(e) = pplns.read().await.save() {
                    error!("Unable to backup pplns: {}", e);
                }
            }
        });

        let res = Arc::new(accounting);

        // payout routine
        #[cfg(feature = "db")]
        task::spawn(Accounting::payout_loop(res.clone()));

        res
    }

    pub fn sender(&self) -> Sender<AccountingMessage> {
        self.sender.clone()
    }

    pub async fn wait_for_exit(&self) {
        while !self.exit_lock.load(std::sync::atomic::Ordering::SeqCst) {
            sleep(Duration::from_millis(100)).await;
        }
    }

    fn pplns_to_provers_shares(pplns: &PPLNS) -> (u32, HashMap<String, u64>) {
        let mut address_shares = HashMap::new();

        let time = Instant::now();
        pplns.queue.iter().for_each(|share| {
            if let Some(shares) = address_shares.get_mut(&share.owner) {
                *shares += share.value;
            } else {
                address_shares.insert(share.clone().owner, share.value);
            }
        });
        debug!("PPLNS to Provers shares took {} us", time.elapsed().as_micros());

        (address_shares.len() as u32, address_shares)
    }

    pub async fn current_round(&self) -> Value {
        let pplns = self.pplns.clone().read().await.clone();
        let cache = self.round_cache.read().await.get(Null {});
        let (provers, shares) = match cache {
            Some(cache) => cache,
            None => {
                let result = Accounting::pplns_to_provers_shares(&pplns);
                self.round_cache.write().await.set(Null {}, result.clone());
                result
            }
        };
        json!({
            "n": pplns.n,
            "current_n": pplns.current_n,
            "provers": provers,
            "shares": shares,
        })
    }

    async fn get_latest_block_height(&self) -> Result<u32> {
        let client = reqwest::Client::new();
        Ok(client
            .post(format!("http://{}:3032", self.operator))
            .json(&json!({
                "jsonrpc": "2.0",
                "method": "latestblockheight",
                "params": [],
                "id": 1,
            }))
            .send()
            .await?
            .json::<Value>()
            .await?["result"]
            .as_u64()
            .ok_or_else(|| anyhow!("Unable to get latest block height"))? as u32)
    }

    #[cfg(feature = "db")]
    async fn update_block_canonical(
        &self,
        height: u32,
        block_hash: String,
        was_canonical: bool,
        reward: i64,
    ) -> Result<bool> {
        let client = reqwest::Client::new();
        let jsonrpc = json!({
            "jsonrpc": "2.0",
            "method": "getminedblockinfo",
            "id": 1,
        });
        let object = match jsonrpc.clone() {
            Value::Object(object) => object,
            _ => unreachable!(),
        };
        let mut request_json = object;
        request_json.insert("params".into(), json!([height, block_hash]));

        let result = &client
            .post(format!("http://{}:3032", self.operator))
            .json(&request_json)
            .send()
            .await?
            .json::<Value>()
            .await?["result"];
        let node_canonical = result["canonical"].as_bool().ok_or_else(|| anyhow!("canonical"))?;
        let value = result["value"].as_i64().ok_or_else(|| anyhow!("value"))?;
        if node_canonical != was_canonical {
            self.database
                .set_block_canonical(block_hash.clone(), node_canonical)
                .await?;
        }
        if node_canonical && value != reward {
            bail!("Block reward mismatch {} {} {}", height, block_hash, reward);
        }
        Ok(node_canonical)
    }

    pub async fn blocks_mined(&self, limit: u16, page: u16, with_shares: bool) -> Result<Value> {
        if !cfg!(feature = "db") {
            return Ok(json!({ "blocks": Vec::<Value>::new() }));
        }

        let latest_block_height: u32 = self.get_latest_block_height().await?;
        let mut obj = serde_json::Map::new();
        #[cfg(feature = "db")]
        let blocks = self.database.get_blocks(limit, page).await?;
        let shares = match with_shares {
            true =>
            {
                #[cfg(feature = "db")]
                self.database
                    .get_block_shares(blocks.iter().map(|b| b.0).collect())
                    .await?
            }
            false => Default::default(),
        };

        let mut res = Vec::<Value>::with_capacity(limit as usize);

        #[cfg(feature = "db")]
        for (id, height, block_hash, mut is_canonical, reward) in blocks {
            let cache_result = self.block_canonical_cache.read().await.get(block_hash.clone());
            if cache_result.is_none() {
                is_canonical = self
                    .update_block_canonical(height, block_hash.clone(), is_canonical, reward)
                    .await?;
                self.block_canonical_cache
                    .write()
                    .await
                    .set(block_hash.clone(), is_canonical);
            }
            let mut value = json!({
                "height": height,
                "block_hash": block_hash.to_string(),
                "confirmed": if is_canonical {
                    // TODO: check consensus
                    true
                } else {
                    false
                },
                "canonical": is_canonical,
                "value": if is_canonical { reward } else { 0 },
            });
            if with_shares {
                let data = shares.get(&id);
                if data == None {
                    bail!("Missing shares for block {}", id);
                }
                value["shares"] = json!(data.unwrap());
            }
            res.push(value);
        }

        obj.insert("blocks".into(), json!(res));
        Ok(obj.into())
    }

    #[cfg(feature = "db")]
    async fn payout_loop(self: Arc<Accounting>) {
        'indef: loop {
            let latest_block_height = match self.get_latest_block_height().await {
                Ok(height) => height,
                Err(e) => {
                    error!("Unable to get latest block height: {}", e);
                    sleep(PAY_INTERVAL).await;
                    continue;
                }
            };
            let blocks = self.database.get_should_pay_blocks(latest_block_height).await;
            if blocks.is_err() {
                error!("Unable to get should pay blocks: {}", blocks.unwrap_err());
                sleep(PAY_INTERVAL).await;
                continue;
            }
            for (id, height, block_hash, is_canonical, reward) in blocks.unwrap() {
                let node_canonical = self
                    .update_block_canonical(height, block_hash.clone(), is_canonical, reward)
                    .await;
                if node_canonical.is_err() {
                    error!("Unable to update block canonical: {}", node_canonical.unwrap_err());
                    sleep(PAY_INTERVAL).await;
                    continue 'indef;
                }
                let canonical = node_canonical.unwrap();
                self.block_canonical_cache
                    .write()
                    .await
                    .set(block_hash.clone(), canonical);
                if canonical {
                    match self.database.pay_block(id).await {
                        Ok(_) => {
                            info!("Paid block {} ({})", height, block_hash);
                        }
                        Err(e) => {
                            error!("Unable to pay block {}: {}", id, e);
                            sleep(PAY_INTERVAL).await;
                            continue 'indef;
                        }
                    }
                }
            }
            if let Err(e) = self.database.set_checked_blocks(latest_block_height).await {
                error!("Unable to set checked blocks: {}", e);
            }

            sleep(PAY_INTERVAL).await;
        }
    }
}
