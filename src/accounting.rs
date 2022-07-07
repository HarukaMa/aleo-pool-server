use std::{
    collections::{HashMap, VecDeque},
    sync::{atomic::AtomicBool, Arc},
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Result};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use snarkvm::dpc::AleoAmount;
use snarkvm::{dpc::testnet2::Testnet2, traits::Network};
use tokio::{
    sync::{
        mpsc::{channel, Sender},
        RwLock as TokioRwLock,
    },
    task,
    time::sleep,
};
use tracing::{debug, error, info};

use crate::{
    accounting::AccountingMessage::NewShare,
    cache::Cache,
    db::DB,
    state_storage::{Storage, StorageData, StorageType},
    AccountingMessage::{Exit, NewBlock, SetN},
};

trait PayoutModel {
    fn add_share(&mut self, share: Share);
}

#[derive(Serialize, Deserialize, Clone)]
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
#[derive(Serialize, Deserialize, Clone)]
struct PPLNS {
    queue: VecDeque<Share>,
    current_n: Arc<RwLock<u64>>,
    n: Arc<RwLock<u64>>,
}

impl PPLNS {
    pub fn load(pplns_storage: StorageData<Null, PPLNS>) -> Self {
        match pplns_storage.get(&Null {}) {
            Ok(Some(pplns)) => pplns,
            Ok(None) | Err(_) => PPLNS {
                queue: VecDeque::new(),
                current_n: Default::default(),
                n: Default::default(),
            },
        }
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
    NewBlock(u32, <Testnet2 as Network>::BlockHash, AleoAmount),
    Exit,
}

#[allow(clippy::type_complexity)]
pub struct Accounting {
    operator: String,
    pplns: Arc<TokioRwLock<PPLNS>>,
    pplns_storage: StorageData<Null, PPLNS>,
    database: Arc<DB>,
    sender: Sender<AccountingMessage>,
    round_cache: TokioRwLock<Cache<Null, (u32, HashMap<String, u64>)>>,
    block_canonical_cache: TokioRwLock<Cache<String, bool>>,
    exit_lock: Arc<AtomicBool>,
}

impl Accounting {
    pub fn init(operator: String) -> Arc<Accounting> {
        let storage = Storage::load();
        let pplns_storage = storage.init_data(StorageType::PPLNS);
        let database = Arc::new(DB::init());

        let pplns = Arc::new(TokioRwLock::new(PPLNS::load(pplns_storage.clone())));

        let (sender, mut receiver) = channel(1024);

        let operator_host = operator.split(':').collect::<Vec<&str>>()[0];
        let accounting = Accounting {
            operator: operator_host.into(),
            pplns,
            pplns_storage,
            database,
            sender,
            round_cache: TokioRwLock::new(Cache::new(Duration::from_secs(10))),
            block_canonical_cache: TokioRwLock::new(Cache::new(Duration::from_secs(300))),
            exit_lock: Arc::new(AtomicBool::new(false)),
        };

        let pplns = accounting.pplns.clone();
        let pplns_storage = accounting.pplns_storage.clone();
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

                        if let Err(e) = database.save_block(height, block_hash, reward, address_shares).await {
                            error!("Failed to save block reward : {}", e);
                        } else {
                            info!("Recorded block {}", height);
                        }
                    }
                    Exit => {
                        receiver.close();
                        let _ = pplns_storage.put(&Null {}, &pplns.read().await.clone());
                        exit_lock.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                }
            }
        });

        // backup pplns
        let pplns = accounting.pplns.clone();
        let pplns_storage = accounting.pplns_storage.clone();
        task::spawn(async move {
            loop {
                sleep(Duration::from_secs(60)).await;
                if let Err(e) = pplns_storage.put(&Null {}, &pplns.read().await.clone()) {
                    error!("Unable to backup pplns: {}", e);
                }
            }
        });

        Arc::new(accounting)
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

    pub async fn blocks_mined(&self, limit: u16, page: u16, with_shares: bool) -> Result<Value> {
        let client = reqwest::Client::new();
        let latest_block_height: u32 = client
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
            .ok_or_else(|| anyhow!("Unable to get latest block height"))? as u32;
        let mut obj = serde_json::Map::new();
        let jsonrpc = json!({
            "jsonrpc": "2.0",
            "method": "getminedblockinfo",
            "id": 1,
        });
        let blocks = self.database.get_blocks(limit, page).await?;
        let shares = match with_shares {
            true => {
                self.database
                    .get_block_shares(blocks.iter().map(|b| b.0).collect())
                    .await?
            }
            false => Default::default(),
        };

        let mut res = Vec::with_capacity(limit as usize);

        for (id, height, block_hash, mut is_canonical, reward) in blocks {
            let cache_result = self.block_canonical_cache.read().await.get(block_hash.clone());
            if cache_result.is_none() {
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
                if node_canonical != is_canonical {
                    self.database
                        .set_block_canonical(block_hash.clone(), node_canonical)
                        .await?;
                    is_canonical = node_canonical;
                }
                if is_canonical && result["value"].as_i64().ok_or_else(|| anyhow!("value"))? != reward {
                    bail!("Block reward mismatch {} {} {}", height, block_hash, reward);
                }
                self.block_canonical_cache
                    .write()
                    .await
                    .set(block_hash.clone(), is_canonical);
            }
            let mut value = json!({
                "height": height,
                "block_hash": block_hash.to_string(),
                "confirmed": if is_canonical {
                    latest_block_height.saturating_sub(Testnet2::ALEO_MAXIMUM_FORK_DEPTH) >= height
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
}
