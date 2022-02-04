use std::{
    collections::VecDeque,
    sync::{atomic::AtomicBool, Arc},
    time::Instant,
};

use serde::{Deserialize, Serialize};
use snarkvm::{
    dpc::{testnet2::Testnet2, Address},
    traits::Network,
};
use tokio::{
    sync::{
        mpsc::{channel, Sender},
        RwLock,
    },
    task,
    time::sleep,
};
use tracing::{debug, error};

use crate::{
    accounting::AccountingMessage::NewShare,
    db::{Storage, StorageData, StorageType},
    AccountingMessage::{Exit, SetN},
};

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
    current_n: u64,
    n: u64,
}

impl PPLNS {
    pub fn load(pplns_storage: StorageData<Null, PPLNS>) -> Self {
        match pplns_storage.get(&Null {}) {
            Ok(Some(pplns)) => pplns,
            Ok(None) | Err(_) => PPLNS {
                queue: VecDeque::new(),
                current_n: 0,
                n: 0,
            },
        }
    }

    pub fn set_n(&mut self, n: u64) {
        let start = Instant::now();
        if n < self.n {
            while self.current_n > n {
                let share = self.queue.pop_front().unwrap();
                self.current_n -= share.value;
            }
        }
        self.n = n;
        debug!("set_n took {} us", start.elapsed().as_micros());
    }

    pub fn add_share(&mut self, share: Share) {
        self.queue.push_back(share);
        self.current_n += share.value;
        while self.current_n > self.n {
            let share = self.queue.pop_front().unwrap();
            self.current_n -= share.value;
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct Null {}

pub enum AccountingMessage {
    NewShare(Address<Testnet2>, u64),
    SetN(u64),
    Exit,
}

pub struct Accounting {
    pplns: Arc<RwLock<PPLNS>>,
    pplns_storage: StorageData<Null, PPLNS>,
    block_reward_storage: StorageData<<Testnet2 as Network>::Commitment, PPLNS>,
    sender: Sender<AccountingMessage>,
    exit_lock: Arc<AtomicBool>,
}

impl Accounting {
    pub fn init() -> Accounting {
        let storage = Storage::load();
        let pplns_storage = storage.init_data(StorageType::PPLNS);
        let block_reward_storage = storage.init_data(StorageType::BlockSnapshot);

        let pplns = Arc::new(RwLock::new(PPLNS::load(pplns_storage.clone())));

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
                    Exit => {
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

        accounting
    }

    pub fn sender(&self) -> Sender<AccountingMessage> {
        self.sender.clone()
    }

    pub async fn wait_for_exit(&self) {
        while !self.exit_lock.load(std::sync::atomic::Ordering::SeqCst) {
            sleep(std::time::Duration::from_millis(100)).await;
        }
    }
}
