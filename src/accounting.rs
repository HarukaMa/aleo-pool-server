use std::{
    collections::{HashMap, VecDeque},
    sync::{atomic::AtomicBool, Arc},
    time::Instant,
};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::json;
use snarkvm::{
    dpc::{testnet2::Testnet2, Address},
    traits::Network,
};
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
    db::{Storage, StorageData, StorageType},
    AccountingMessage::{Exit, NewBlock, SetN},
};

trait PayoutModel {
    fn add_share(&mut self, share: Share);
}

#[derive(Serialize, Deserialize, Copy, Clone)]
struct Share {
    value: u64,
    owner: Address<Testnet2>,
}

impl Share {
    pub fn init(value: u64, owner: Address<Testnet2>) -> Self {
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

    pub fn add_share(&mut self, share: Share) {
        let start = Instant::now();
        self.queue.push_back(share);
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

#[derive(Serialize, Deserialize, Clone)]
struct Null {}

pub enum AccountingMessage {
    NewShare(Address<Testnet2>, u64),
    SetN(u64),
    NewBlock(u32, <Testnet2 as Network>::Commitment),
    Exit,
}

pub struct Accounting {
    pplns: Arc<TokioRwLock<PPLNS>>,
    pplns_storage: StorageData<Null, PPLNS>,
    block_reward_storage: StorageData<(u32, <Testnet2 as Network>::Commitment), PPLNS>,
    sender: Sender<AccountingMessage>,
    exit_lock: Arc<AtomicBool>,
}

impl Accounting {
    pub fn init() -> Arc<Accounting> {
        let storage = Storage::load();
        let pplns_storage = storage.init_data(StorageType::PPLNS);
        let block_reward_storage = storage.init_data(StorageType::BlockSnapshot);

        let pplns = Arc::new(TokioRwLock::new(PPLNS::load(pplns_storage.clone())));

        let (sender, mut receiver) = channel(1024);

        let accounting = Accounting {
            pplns,
            pplns_storage,
            block_reward_storage,
            sender,
            exit_lock: Arc::new(AtomicBool::new(false)),
        };

        let pplns = accounting.pplns.clone();
        let pplns_storage = accounting.pplns_storage.clone();
        let block_reward_storage = accounting.block_reward_storage.clone();
        let exit_lock = accounting.exit_lock.clone();
        task::spawn(async move {
            while let Some(request) = receiver.recv().await {
                match request {
                    NewShare(address, value) => {
                        pplns.write().await.add_share(Share::init(value, address));
                        debug!("Recorded share from {} with value {}", address, value);
                    }
                    SetN(n) => {
                        pplns.write().await.set_n(n);
                        debug!("Set N to {}", n);
                    }
                    NewBlock(height, commitment) => {
                        if let Err(e) = block_reward_storage.put(&(height, commitment), &pplns.read().await.clone()) {
                            error!("Failed to save block reward : {}", e);
                        }
                        info!("Recorded block {}", height);
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
                sleep(std::time::Duration::from_secs(60)).await;
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
            sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    pub async fn current_round(&self) -> serde_json::Value {
        let pplns = self.pplns.clone().read().await.clone();
        let mut address_shares = HashMap::new();
        for share in pplns.queue.iter() {
            if let Some(shares) = address_shares.get_mut(&share.owner) {
                *shares += share.value;
            } else {
                address_shares.insert(share.owner, share.value);
            }
        }
        json!({
            "n": pplns.n,
            "current_n": pplns.current_n,
            "provers": address_shares.len(),
            "shares": address_shares,
        })
    }
}
