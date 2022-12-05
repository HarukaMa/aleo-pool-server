use std::{
    collections::{HashMap, VecDeque},
    fs::create_dir_all,
    path::PathBuf,
    sync::{atomic::AtomicBool, Arc},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Error, Result};
use cache::Cache;
use dirs::home_dir;
use parking_lot::RwLock;
use savefile::{load_file, save_file};
use savefile_derive::Savefile;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use snarkvm::prelude::{PuzzleCommitment, Testnet3};
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
    accounting::AccountingMessage::{NewShare, NewSolution},
    AccountingMessage::{Exit, SetN},
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
        create_dir_all(home.as_ref().unwrap().join(".aleo_pool_testnet3_2")).unwrap();
        let db_path = home.unwrap().join(".aleo_pool_testnet3_2/state");
        if !db_path.exists() {
            return PPLNS {
                queue: VecDeque::new(),
                current_n: Default::default(),
                n: Default::default(),
            };
        }
        load_file::<PPLNS, PathBuf>(db_path, 0).unwrap()
    }

    pub fn save(&self) -> std::result::Result<(), Error> {
        let home = home_dir();
        if home.is_none() {
            panic!("No home directory found");
        }
        let db_path = home.unwrap().join(".aleo_pool_testnet3_2/state");
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
    NewSolution(PuzzleCommitment<Testnet3>),
    Exit,
}

#[cfg(feature = "db")]
static PAY_INTERVAL: Duration = Duration::from_secs(60);

#[allow(clippy::type_complexity)]
pub struct Accounting {
    pplns: Arc<TokioRwLock<PPLNS>>,
    #[cfg(feature = "db")]
    database: Arc<DB>,
    sender: Sender<AccountingMessage>,
    round_cache: TokioRwLock<Cache<Null, (u32, HashMap<String, u64>)>>,
    exit_lock: Arc<AtomicBool>,
}

impl Accounting {
    pub fn init() -> Arc<Accounting> {
        #[cfg(feature = "db")]
        let database = Arc::new(DB::init());

        let pplns = Arc::new(TokioRwLock::new(PPLNS::load()));

        let (sender, mut receiver) = channel(1024);

        let accounting = Accounting {
            pplns,
            #[cfg(feature = "db")]
            database,
            sender,
            round_cache: TokioRwLock::new(Cache::new(Duration::from_secs(10))),
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
                    NewSolution(commitment) => {
                        let pplns = pplns.read().await.clone();
                        let (_, address_shares) = Accounting::pplns_to_provers_shares(&pplns);

                        #[cfg(feature = "db")]
                        if let Err(e) = database.save_solution(commitment, address_shares).await {
                            error!("Failed to save block reward : {}", e);
                        } else {
                            info!("Recorded solution {}", commitment);
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

    #[cfg(feature = "db")]
    async fn check_solution(&self, commitment: &String) -> Result<bool> {
        let client = reqwest::Client::new();

        let result = &client
            .get(format!("http://127.0.0.1:8001/commitment?commitment={}", commitment))
            .send()
            .await?
            .json::<Value>()
            .await?;
        let is_valid = result.as_null().is_none();
        if is_valid {
            self.database
                .set_solution_valid(
                    commitment,
                    true,
                    Some(result["height"].as_u64().ok_or_else(|| anyhow!("height"))? as u32),
                    Some(result["reward"].as_u64().ok_or_else(|| anyhow!("reward"))?),
                )
                .await?;
        } else {
            self.database.set_solution_valid(commitment, false, None, None).await?;
        }
        Ok(is_valid)
    }

    #[cfg(feature = "db")]
    async fn payout_loop(self: Arc<Accounting>) {
        'forever: loop {
            info!("Running payout loop");
            let blocks = self.database.get_should_pay_solutions().await;
            if blocks.is_err() {
                error!("Unable to get should pay blocks: {}", blocks.unwrap_err());
                sleep(PAY_INTERVAL).await;
                continue;
            }
            for (id, commitment) in blocks.unwrap() {
                let valid = self.check_solution(&commitment).await;
                if valid.is_err() {
                    error!("Unable to check solution: {}", valid.unwrap_err());
                    sleep(PAY_INTERVAL).await;
                    continue 'forever;
                }
                let valid = valid.unwrap();
                if valid {
                    match self.database.pay_solution(id).await {
                        Ok(_) => {
                            info!("Paid solution {}", commitment);
                        }
                        Err(e) => {
                            error!("Unable to pay solution {}: {}", id, e);
                            sleep(PAY_INTERVAL).await;
                            continue 'forever;
                        }
                    }
                }
            }

            sleep(PAY_INTERVAL).await;
        }
    }
}
