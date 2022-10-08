use std::{
    collections::{HashMap, HashSet},
    fmt::{Display, Formatter},
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use aleo_stratum::{codec::ResponseParams, message::StratumMessage};
use flurry::HashSet as FlurryHashSet;
use json_rpc_types::{Error, ErrorCode, Id};
use rand::thread_rng;
use snarkvm::{
    compiler::{hash_commitment, CoinbasePuzzle, CoinbaseVerifyingKey, EpochChallenge, PuzzleConfig},
    console::account::address::Address,
    prelude::{Environment, Testnet3, ToBytes},
};
use snarkvm_algorithms::{
    crypto_hash::sha256d_to_u64,
    polycommit::kzg10::{KZGCommitment, KZGProof, UniversalParams, KZG10},
};
use speedometer::Speedometer;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{
        mpsc::{channel, Sender},
        RwLock,
    },
    task,
};
use tracing::{debug, error, info, trace, warn};

use crate::{connection::Connection, AccountingMessage};

struct ProverState {
    peer_addr: SocketAddr,
    address: Address<Testnet3>,
    speed_2m: Speedometer,
    speed_5m: Speedometer,
    speed_15m: Speedometer,
    speed_30m: Speedometer,
    speed_1h: Speedometer,
    current_difficulty: u64,
    next_difficulty: u64,
}

impl ProverState {
    pub fn new(peer_addr: SocketAddr, address: Address<Testnet3>) -> Self {
        Self {
            peer_addr,
            address,
            speed_2m: Speedometer::init(Duration::from_secs(120)),
            speed_5m: Speedometer::init_with_cache(Duration::from_secs(60 * 5), Duration::from_secs(30)),
            speed_15m: Speedometer::init_with_cache(Duration::from_secs(60 * 15), Duration::from_secs(30)),
            speed_30m: Speedometer::init_with_cache(Duration::from_secs(60 * 30), Duration::from_secs(30)),
            speed_1h: Speedometer::init_with_cache(Duration::from_secs(60 * 60), Duration::from_secs(30)),
            current_difficulty: 1,
            next_difficulty: 1,
        }
    }

    pub async fn add_share(&mut self, value: u64) {
        let now = Instant::now();
        self.speed_2m.event(value).await;
        self.speed_5m.event(value).await;
        self.speed_15m.event(value).await;
        self.speed_30m.event(value).await;
        self.speed_1h.event(value).await;
        self.next_difficulty = ((self.speed_2m.speed().await * 20.0) as u64).max(1);
        debug!("add_share took {} us", now.elapsed().as_micros());
    }

    pub async fn next_difficulty(&mut self) -> u64 {
        self.current_difficulty = self.next_difficulty;
        self.current_difficulty
    }

    pub fn current_difficulty(&self) -> u64 {
        self.current_difficulty
    }

    pub fn address(&self) -> Address<Testnet3> {
        self.address
    }

    // noinspection DuplicatedCode
    pub async fn speed(&mut self) -> Vec<f64> {
        vec![
            self.speed_5m.speed().await,
            self.speed_15m.speed().await,
            self.speed_30m.speed().await,
            self.speed_1h.speed().await,
        ]
    }
}

impl Display for ProverState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let addr_str = self.address.to_string();
        write!(
            f,
            "{} ({}...{})",
            self.peer_addr,
            &addr_str[0..11],
            &addr_str[addr_str.len() - 6..]
        )
    }
}

struct PoolState {
    speed_1m: Speedometer,
    speed_5m: Speedometer,
    speed_15m: Speedometer,
    speed_30m: Speedometer,
    speed_1h: Speedometer,
    current_global_difficulty_modifier: f64,
    next_global_difficulty_modifier: f64,
}

impl PoolState {
    pub fn new() -> Self {
        Self {
            speed_1m: Speedometer::init(Duration::from_secs(60)),
            speed_5m: Speedometer::init_with_cache(Duration::from_secs(60 * 5), Duration::from_secs(30)),
            speed_15m: Speedometer::init_with_cache(Duration::from_secs(60 * 15), Duration::from_secs(30)),
            speed_30m: Speedometer::init_with_cache(Duration::from_secs(60 * 30), Duration::from_secs(30)),
            speed_1h: Speedometer::init_with_cache(Duration::from_secs(60 * 60), Duration::from_secs(30)),
            current_global_difficulty_modifier: 1.0,
            next_global_difficulty_modifier: 1.0,
        }
    }

    pub async fn add_share(&mut self, value: u64) {
        let now = Instant::now();
        self.speed_1m.event(value).await;
        self.speed_5m.event(value).await;
        self.speed_15m.event(value).await;
        self.speed_30m.event(value).await;
        self.speed_1h.event(value).await;
        self.next_global_difficulty_modifier = (self.speed_1m.speed().await / 10.0).max(1f64);
        // todo: make adjustable through admin api
        debug!("pool state add_share took {} us", now.elapsed().as_micros());
    }

    pub async fn next_global_difficulty_modifier(&mut self) -> f64 {
        self.current_global_difficulty_modifier = self.next_global_difficulty_modifier;
        self.current_global_difficulty_modifier
    }

    pub fn current_global_difficulty_modifier(&self) -> f64 {
        self.current_global_difficulty_modifier
    }

    // noinspection DuplicatedCode
    pub async fn speed(&mut self) -> Vec<f64> {
        vec![
            self.speed_5m.speed().await,
            self.speed_15m.speed().await,
            self.speed_30m.speed().await,
            self.speed_1h.speed().await,
        ]
    }
}

struct CoinbasePuzzleData {
    srs: UniversalParams<<Testnet3 as Environment>::PairingCurve>,
    vks: HashMap<u32, CoinbaseVerifyingKey<Testnet3>>,
}

impl CoinbasePuzzleData {
    pub fn new() -> Self {
        let rng = &mut thread_rng();
        let max_degree = 1 << 15;
        let max_config = PuzzleConfig { degree: max_degree };
        let universal_srs = CoinbasePuzzle::<Testnet3>::setup(max_config, rng).unwrap();
        Self {
            srs: universal_srs,
            vks: HashMap::new(),
        }
    }

    pub fn init_vk(&mut self, degree: u32) {
        info!("Generating new coinbase verifying key for degree {}", degree);
        let config = PuzzleConfig { degree };
        let vk = CoinbasePuzzle::<Testnet3>::trim(&self.srs, config).unwrap();
        self.vks.insert(degree, vk.1);
        info!("Done generating new coinbase verifying key for degree {}", degree);
    }

    pub fn get_vk(&self, degree: u32) -> &CoinbaseVerifyingKey<Testnet3> {
        self.vks.get(&degree).unwrap()
    }
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum ServerMessage {
    ProverConnected(TcpStream, SocketAddr),
    ProverAuthenticated(SocketAddr, Address<Testnet3>, Sender<StratumMessage>),
    ProverDisconnected(SocketAddr),
    ProverSubmit(
        Id,
        SocketAddr,
        u64,
        u64,
        KZGCommitment<<Testnet3 as Environment>::PairingCurve>,
        KZGProof<<Testnet3 as Environment>::PairingCurve>,
    ),
    NewEpochChallenge(EpochChallenge<Testnet3>, u64, u64),
    Exit,
}

impl ServerMessage {
    fn name(&self) -> &'static str {
        match self {
            ServerMessage::ProverConnected(..) => "ProverConnected",
            ServerMessage::ProverAuthenticated(..) => "ProverAuthenticated",
            ServerMessage::ProverDisconnected(..) => "ProverDisconnected",
            ServerMessage::ProverSubmit(..) => "ProverSubmit",
            ServerMessage::NewEpochChallenge(..) => "NewEpochChallenge",
            ServerMessage::Exit => "Exit",
        }
    }
}

impl Display for ServerMessage {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

pub struct Server {
    sender: Sender<ServerMessage>,
    operator_sender: (),
    accounting_sender: Sender<AccountingMessage>,
    pool_address: Address<Testnet3>,
    connected_provers: RwLock<HashSet<SocketAddr>>,
    authenticated_provers: Arc<RwLock<HashMap<SocketAddr, Sender<StratumMessage>>>>,
    pool_state: Arc<RwLock<PoolState>>,
    prover_states: Arc<RwLock<HashMap<SocketAddr, RwLock<ProverState>>>>,
    prover_address_connections: Arc<RwLock<HashMap<Address<Testnet3>, HashSet<SocketAddr>>>>,
    coinbase_puzzle_data: Arc<RwLock<CoinbasePuzzleData>>,
    latest_epoch_number: AtomicU64,
    latest_epoch_challenge: Arc<RwLock<Option<EpochChallenge<Testnet3>>>>,
    latest_proof_target: AtomicU64,
    nonce_seen: Arc<FlurryHashSet<u64>>,
}

impl Server {
    pub async fn init(
        port: u16,
        address: Address<Testnet3>,
        operator_sender: (),
        accounting_sender: Sender<AccountingMessage>,
    ) -> Arc<Server> {
        let (sender, mut receiver) = channel(1024);

        let (_, listener) = match TcpListener::bind(format!("0.0.0.0:{}", port)).await {
            Ok(listener) => {
                let local_ip = listener.local_addr().expect("Could not get local ip");
                info!("Listening on {}", local_ip);
                (local_ip, listener)
            }
            Err(e) => {
                panic!("Unable to start the server: {:?}", e);
            }
        };

        info!("Initializing coinbase puzzle data");
        let coinbase_puzzle_data = CoinbasePuzzleData::new();
        info!("Coinbase puzzle data initialized");

        let server = Arc::new(Server {
            sender,
            operator_sender,
            accounting_sender,
            pool_address: address,
            connected_provers: Default::default(),
            authenticated_provers: Default::default(),
            pool_state: Arc::new(RwLock::new(PoolState::new())),
            prover_states: Default::default(),
            prover_address_connections: Default::default(),
            coinbase_puzzle_data: Arc::new(RwLock::new(coinbase_puzzle_data)),
            latest_epoch_number: AtomicU64::new(0),
            latest_epoch_challenge: Default::default(),
            latest_proof_target: AtomicU64::new(0),
            nonce_seen: Arc::new(FlurryHashSet::with_capacity(10 << 20)),
        });

        // clear nonce
        {
            let nonce = server.nonce_seen.clone();
            let mut ticker = tokio::time::interval(Duration::from_secs(60));
            task::spawn(async move {
                loop {
                    ticker.tick().await;
                    nonce.pin().clear()
                }
            });
        }

        let s = server.clone();
        task::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, peer_addr)) => {
                        info!("New connection from: {}", peer_addr);
                        if let Err(e) = s.sender.send(ServerMessage::ProverConnected(stream, peer_addr)).await {
                            error!("Error accepting connection: {}", e);
                        }
                    }
                    Err(e) => {
                        error!("Error accepting connection: {:?}", e);
                    }
                }
            }
        });

        let s = server.clone();
        task::spawn(async move {
            let server = s.clone();
            while let Some(msg) = receiver.recv().await {
                let server = server.clone();
                task::spawn(async move {
                    server.process_message(msg).await;
                });
            }
        });

        let ss = server.clone();
        task::spawn(async move {
            ss.sender
                .send(ServerMessage::NewEpochChallenge(
                    EpochChallenge::new(0, Default::default(), (1 << 13) - 1).unwrap(),
                    1 << 11,
                    1 << 1,
                ))
                .await
                .expect("Could not send fake epoch challenge");
        });

        server
    }

    fn seen_nonce(nonce_seen: Arc<FlurryHashSet<u64>>, nonce: u64) -> bool {
        !nonce_seen.pin().insert(nonce)
    }

    pub fn sender(&self) -> Sender<ServerMessage> {
        self.sender.clone()
    }

    pub async fn process_message(&self, msg: ServerMessage) {
        trace!("Received message: {}", msg);
        match msg {
            ServerMessage::ProverConnected(stream, peer_addr) => {
                self.connected_provers.write().await.insert(peer_addr);
                Connection::init(stream, peer_addr, self.sender.clone(), self.pool_address).await;
            }
            ServerMessage::ProverAuthenticated(peer_addr, address, sender) => {
                self.authenticated_provers
                    .write()
                    .await
                    .insert(peer_addr, sender.clone());
                self.prover_states
                    .write()
                    .await
                    .insert(peer_addr, ProverState::new(peer_addr, address).into());
                let mut pac_write = self.prover_address_connections.write().await;
                if let Some(address) = pac_write.get_mut(&address) {
                    address.insert(peer_addr);
                } else {
                    pac_write.insert(address, HashSet::from([peer_addr]));
                }
                drop(pac_write);
                if let Err(e) = sender.send(StratumMessage::SetTarget(u64::MAX)).await {
                    error!("Error sending initial target to prover: {}", e);
                }
                if let Some(epoch_challenge) = self.latest_epoch_challenge.read().await.as_ref() {
                    let job_id = hex::encode(self.latest_epoch_number.load(Ordering::SeqCst).to_le_bytes());
                    if let Err(e) = sender
                        .send(StratumMessage::Notify(
                            job_id,
                            hex::encode(epoch_challenge.to_bytes_le().unwrap()),
                            None,
                            true,
                        ))
                        .await
                    {
                        error!(
                            "Error sending initial epoch challenge to prover {} ({}): {}",
                            peer_addr, address, e
                        );
                    }
                }
            }
            ServerMessage::ProverDisconnected(peer_addr) => {
                let state = self.prover_states.write().await.remove(&peer_addr);
                let address = match state {
                    Some(state) => Some(state.read().await.address()),
                    None => None,
                };
                if address.is_some() {
                    let mut pac_write = self.prover_address_connections.write().await;
                    let pac = pac_write.get_mut(&address.unwrap());
                    if let Some(pac) = pac {
                        pac.remove(&peer_addr);
                        if pac.is_empty() {
                            pac_write.remove(&address.unwrap());
                        }
                    }
                }
                self.connected_provers.write().await.remove(&peer_addr);
                self.authenticated_provers.write().await.remove(&peer_addr);
            }
            ServerMessage::NewEpochChallenge(epoch_challenge, coinbase_target, proof_target) => {
                info!("New epoch challenge: {}", epoch_challenge.epoch_number());
                // pre-generate coinbase verifying key
                self.coinbase_puzzle_data
                    .write()
                    .await
                    .init_vk(epoch_challenge.degree().unwrap());
                self.latest_epoch_number
                    .store(epoch_challenge.epoch_number(), Ordering::SeqCst);
                self.latest_epoch_challenge
                    .write()
                    .await
                    .replace(epoch_challenge.clone());
                self.latest_proof_target.store(proof_target, Ordering::SeqCst);
                if let Err(e) = self
                    .accounting_sender
                    .send(AccountingMessage::SetN(coinbase_target * 5))
                    .await
                {
                    error!("Error sending accounting message: {}", e);
                }
                let global_difficulty_modifier = self.pool_state.write().await.next_global_difficulty_modifier().await;
                debug!("Global difficulty modifier: {}", global_difficulty_modifier);
                let job_id = hex::encode(epoch_challenge.epoch_number().to_le_bytes());
                let epoch_challenge_hex = hex::encode(epoch_challenge.to_bytes_le().unwrap());
                for (peer_addr, sender) in self.authenticated_provers.read().await.clone().iter() {
                    let states = self.prover_states.read().await;
                    let prover_state = match states.get(peer_addr) {
                        Some(state) => state,
                        None => {
                            error!("Prover state not found for peer: {}", peer_addr);
                            continue;
                        }
                    };

                    let prover_display = format!("{}", prover_state.read().await);
                    let current_difficulty = prover_state.read().await.current_difficulty();
                    let mut next_difficulty =
                        (prover_state.write().await.next_difficulty().await as f64 * global_difficulty_modifier) as u64;
                    drop(states);
                    if next_difficulty > proof_target {
                        next_difficulty = proof_target;
                    }
                    if current_difficulty != next_difficulty {
                        if let Err(e) = sender.send(StratumMessage::SetTarget(next_difficulty)).await {
                            error!("Error sending difficulty target to prover {}: {}", prover_display, e);
                        }
                    }
                    if let Err(e) = sender
                        .send(StratumMessage::Notify(
                            job_id.clone(),
                            epoch_challenge_hex.clone(),
                            None,
                            true,
                        ))
                        .await
                    {
                        error!("Error sending block template to prover {}: {}", prover_display, e);
                    }
                }
            }
            ServerMessage::ProverSubmit(id, peer_addr, epoch_number, nonce, commitment, proof) => {
                let prover_states = self.prover_states.clone();
                let pool_state = self.pool_state.clone();
                let authenticated_provers = self.authenticated_provers.clone();
                let latest_epoch_number = self.latest_epoch_number.load(Ordering::SeqCst);
                let current_global_difficulty_modifier =
                    self.pool_state.read().await.current_global_difficulty_modifier();
                let latest_epoch_challenge = self.latest_epoch_challenge.clone();
                let accounting_sender = self.accounting_sender.clone();
                // let operator_sender = self.operator_sender.clone();
                let seen_nonce = self.nonce_seen.clone();
                let global_proof_target = self.latest_proof_target.load(Ordering::SeqCst);
                let pool_address = self.pool_address;
                let coinbase_puzzle_data = self.coinbase_puzzle_data.clone();
                task::spawn(async move {
                    async fn send_result(
                        sender: &Sender<StratumMessage>,
                        id: Id,
                        result: bool,
                        error_code: Option<ErrorCode>,
                        desc: Option<String>,
                    ) {
                        if result {
                            if let Err(e) = sender
                                .send(StratumMessage::Response(id, Some(ResponseParams::Bool(true)), None))
                                .await
                            {
                                error!("Error sending result to prover: {}", e);
                            }
                        } else if let Err(e) = sender
                            .send(StratumMessage::Response(
                                id,
                                None,
                                Some(Error::with_custom_msg(error_code.unwrap(), desc.unwrap().as_str())),
                            ))
                            .await
                        {
                            error!("Error sending result to prover: {}", e);
                        }
                    }
                    let states = prover_states.read().await;
                    let provers = authenticated_provers.read().await;
                    let sender = match provers.get(&peer_addr) {
                        Some(sender) => sender,
                        None => {
                            error!("Sender not found for peer: {}", peer_addr);
                            return;
                        }
                    };
                    let prover_state = match states.get(&peer_addr) {
                        Some(state) => state,
                        None => {
                            error!("Received proof from unknown prover: {}", peer_addr);
                            send_result(
                                sender,
                                id,
                                false,
                                Some(ErrorCode::from_code(24)),
                                Some("Unknown prover".to_string()),
                            )
                            .await;
                            return;
                        }
                    };
                    let prover_display = format!("{}", prover_state.read().await);
                    let epoch_challenge = match latest_epoch_challenge.read().await.clone() {
                        Some(template) => template,
                        None => {
                            warn!(
                                "Received proof from prover {} while no epoch challenge is available",
                                prover_display
                            );
                            send_result(
                                sender,
                                id,
                                false,
                                Some(ErrorCode::from_code(21)),
                                Some("No epoch challenge".to_string()),
                            )
                            .await;
                            return;
                        }
                    };
                    if epoch_number != latest_epoch_number {
                        info!(
                            "Received stale proof from prover {} with epoch number: {} (expected {})",
                            prover_display, epoch_number, latest_epoch_number
                        );
                        send_result(
                            sender,
                            id,
                            false,
                            Some(ErrorCode::from_code(21)),
                            Some("Stale proof".to_string()),
                        )
                        .await;
                        return;
                    }
                    if Self::seen_nonce(seen_nonce, nonce) {
                        warn!("Received duplicate nonce from prover {}", prover_display);
                        send_result(
                            sender,
                            id,
                            false,
                            Some(ErrorCode::from_code(22)),
                            Some("Duplicate nonce".to_string()),
                        )
                        .await;
                        return;
                    }
                    let mut difficulty_target = (prover_state.read().await.current_difficulty() as f64
                        * current_global_difficulty_modifier) as u64;
                    if difficulty_target > global_proof_target {
                        difficulty_target = global_proof_target;
                    }
                    let proof_difficulty = match &commitment.to_bytes_le() {
                        Ok(bytes) => u64::MAX / sha256d_to_u64(bytes),
                        Err(e) => {
                            warn!("Received invalid proof from prover {}: {}", prover_display, e);
                            send_result(
                                sender,
                                id,
                                false,
                                Some(ErrorCode::from_code(23)),
                                Some("Invalid proof".to_string()),
                            )
                            .await;
                            return;
                        }
                    };
                    if proof_difficulty < difficulty_target {
                        warn!(
                            "Received proof with difficulty {} from prover {} (expected {})",
                            proof_difficulty, prover_display, difficulty_target
                        );
                        send_result(
                            sender,
                            id,
                            false,
                            Some(ErrorCode::from_code(23)),
                            Some("Difficulty target not met".to_string()),
                        )
                        .await;
                        return;
                    }
                    info!("Verifying proof from prover {}", prover_display);
                    let polynomial = match CoinbasePuzzle::prover_polynomial(&epoch_challenge, &pool_address, nonce) {
                        Ok(polynomial) => polynomial,
                        Err(e) => {
                            warn!(
                                "Failed to construct prover polynomial from prover {}: {}",
                                prover_display, e
                            );
                            send_result(
                                sender,
                                id,
                                false,
                                Some(ErrorCode::from_code(20)),
                                Some("Invalid polynomial".to_string()),
                            )
                            .await;
                            return;
                        }
                    };
                    let point = match hash_commitment(&commitment) {
                        Ok(point) => point,
                        Err(e) => {
                            warn!("Failed to hash commitment from prover {}: {}", prover_display, e);
                            send_result(
                                sender,
                                id,
                                false,
                                Some(ErrorCode::from_code(20)),
                                Some("Invalid commitment".to_string()),
                            )
                            .await;
                            return;
                        }
                    };
                    let product_eval_at_point =
                        polynomial.evaluate(point) * epoch_challenge.epoch_polynomial().evaluate(point);
                    let cpd = coinbase_puzzle_data.read().await;
                    let verifying_key = cpd.get_vk(epoch_challenge.degree().unwrap()).clone();
                    drop(cpd);
                    match KZG10::check(&verifying_key, &commitment, point, product_eval_at_point, &proof) {
                        Ok(true) => {
                            info!("Verified proof from prover {}", prover_display);
                        }
                        _ => {
                            warn!("Failed to verify proof from prover {}", prover_display);
                            send_result(
                                sender,
                                id,
                                false,
                                Some(ErrorCode::from_code(20)),
                                Some("Invalid proof".to_string()),
                            )
                            .await;
                            return;
                        }
                    }

                    prover_state.write().await.add_share(proof_difficulty).await;
                    pool_state.write().await.add_share(proof_difficulty).await;
                    if let Err(e) = accounting_sender
                        .send(AccountingMessage::NewShare(
                            prover_state.read().await.address().to_string(),
                            proof_difficulty,
                        ))
                        .await
                    {
                        error!("Failed to send accounting message: {}", e);
                    }
                    send_result(sender, id, true, None, None).await;
                    drop(provers);
                    drop(states);
                    info!(
                        "Received valid proof from prover {} with difficulty {}",
                        prover_display, proof_difficulty
                    );
                    // TODO: testnet3 rewards
                    // if proof_difficulty <= epoch_challenge.difficulty_target() {
                    //     info!(
                    //         "Received unconfirmed block from prover {} with difficulty {} (target {})",
                    //         prover_display,
                    //         proof_difficulty,
                    //         epoch_challenge.difficulty_target()
                    //     );
                    //     TODO: dummy operator
                    //     if let Err(e) = operator_sender
                    //         .send(OperatorMessage::PoolBlock(nonce, Data::Object(proof)))
                    //         .await
                    //     {
                    //         error!("Failed to report unconfirmed block to operator: {}", e);
                    //     }
                    //     let reward = epoch_challenge.coinbase_record().value();
                    //     TODO: dummy accounting
                    //     match epoch_challenge.to_header_root() {
                    //         Ok(header_root) => match &to_bytes_le![block_template.previous_block_hash(), header_root] {
                    //             Ok(block_hash_bytes) => match Testnet2::block_hash_crh().hash(block_hash_bytes) {
                    //                 Ok(block_hash) => {
                    //                     if let Err(e) = {
                    //                         accounting_sender
                    //                             .send(AccountingMessage::NewBlock(
                    //                                 block_height,
                    //                                 block_hash.into(),
                    //                                 reward,
                    //                             ))
                    //                             .await
                    //                     } {
                    //                         error!("Failed to send accounting message: {}", e);
                    //                     }
                    //                 }
                    //                 Err(e) => {
                    //                     error!("Failed to calculate block hash: {}", e);
                    //                 }
                    //             },
                    //             Err(e) => error!("Failed to convert header root to bytes: {}", e),
                    //         },
                    //         Err(e) => {
                    //             error!("Failed to get header root: {}", e);
                    //         }
                    //     }
                    // }
                });
            }
            ServerMessage::Exit => {}
        }
    }

    pub async fn online_provers(&self) -> u32 {
        self.authenticated_provers.read().await.len() as u32
    }

    pub async fn online_addresses(&self) -> u32 {
        self.prover_address_connections.read().await.len() as u32
    }

    pub async fn pool_speed(&self) -> Vec<f64> {
        self.pool_state.write().await.speed().await
    }

    pub async fn address_prover_count(&self, address: Address<Testnet3>) -> u32 {
        self.prover_address_connections
            .read()
            .await
            .get(&address)
            .map(|prover_connections| prover_connections.len() as u32)
            .unwrap_or(0)
    }

    pub async fn address_speed(&self, address: Address<Testnet3>) -> Vec<f64> {
        let mut speed = vec![0.0, 0.0, 0.0, 0.0];
        let prover_connections_lock = self.prover_address_connections.read().await;
        let prover_connections = prover_connections_lock.get(&address);
        if prover_connections.is_none() {
            return speed;
        }
        for prover_connection in prover_connections.unwrap() {
            if let Some(prover_state) = self.prover_states.read().await.get(prover_connection) {
                let mut prover_state_lock = prover_state.write().await;
                prover_state_lock
                    .speed()
                    .await
                    .iter()
                    .zip(speed.iter_mut())
                    .for_each(|(s, speed)| {
                        *speed += s;
                    });
            }
        }
        speed
    }

    // pub async fn check
}
