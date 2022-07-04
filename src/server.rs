use snarkos::environment::network::Data;
use std::{
    collections::{HashMap, HashSet},
    fmt::{Display, Formatter},
    net::SocketAddr,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use snarkvm::{
    dpc::{testnet2::Testnet2, Address, BlockTemplate, PoSWProof, PoSWScheme},
    traits::Network,
    utilities::{to_bytes_le, ToBytes},
};
use snarkvm_algorithms::traits::crh::CRH;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{
        mpsc::{channel, Sender},
        RwLock,
    },
    task,
};
use tracing::{debug, error, info, trace, warn};

use crate::{
    connection::Connection, message::ProverMessage, operator_peer::OperatorMessage, speedometer::Speedometer,
    AccountingMessage,
};

struct ProverState {
    peer_addr: SocketAddr,
    address: Address<Testnet2>,
    speed_1m: Speedometer,
    speed_5m: Speedometer,
    speed_15m: Speedometer,
    speed_30m: Speedometer,
    speed_1h: Speedometer,
    current_difficulty: u64,
    next_difficulty: u64,
    nonce_seen: HashSet<<Testnet2 as Network>::PoSWNonce>,
}

impl ProverState {
    pub fn new(peer_addr: SocketAddr, address: Address<Testnet2>) -> Self {
        Self {
            peer_addr,
            address,
            speed_1m: Speedometer::init(Duration::from_secs(60)),
            speed_5m: Speedometer::init_with_cache(Duration::from_secs(60 * 5), Duration::from_secs(30)),
            speed_15m: Speedometer::init_with_cache(Duration::from_secs(60 * 15), Duration::from_secs(30)),
            speed_30m: Speedometer::init_with_cache(Duration::from_secs(60 * 30), Duration::from_secs(30)),
            speed_1h: Speedometer::init_with_cache(Duration::from_secs(60 * 60), Duration::from_secs(30)),
            current_difficulty: 1,
            next_difficulty: 1,
            nonce_seen: HashSet::new(),
        }
    }

    pub async fn add_share(&mut self, value: u64) {
        let now = Instant::now();
        self.speed_1m.event(value).await;
        self.speed_5m.event(value).await;
        self.speed_15m.event(value).await;
        self.speed_30m.event(value).await;
        self.speed_1h.event(value).await;
        self.next_difficulty = ((self.speed_1m.speed().await * 20.0) as u64).max(1);
        debug!("add_share took {} us", now.elapsed().as_micros());
    }

    pub async fn next_difficulty(&mut self) -> u64 {
        self.current_difficulty = self.next_difficulty;
        self.current_difficulty
    }

    pub fn current_difficulty(&self) -> u64 {
        self.current_difficulty
    }

    pub fn seen_nonce(&mut self, nonce: <Testnet2 as Network>::PoSWNonce) -> bool {
        !self.nonce_seen.insert(nonce)
    }

    pub fn address(&self) -> Address<Testnet2> {
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

#[allow(clippy::large_enum_variant)]
pub enum ServerMessage {
    ProverConnected(TcpStream, SocketAddr),
    ProverAuthenticated(SocketAddr, Address<Testnet2>, Sender<ProverMessage>),
    ProverDisconnected(SocketAddr),
    ProverSubmit(SocketAddr, u32, <Testnet2 as Network>::PoSWNonce, PoSWProof<Testnet2>),
    NewBlockTemplate(BlockTemplate<Testnet2>),
    Exit,
}

impl ServerMessage {
    fn name(&self) -> &'static str {
        match self {
            ServerMessage::ProverConnected(_, _) => "ProverConnected",
            ServerMessage::ProverAuthenticated(_, _, _) => "ProverAuthenticated",
            ServerMessage::ProverDisconnected(_) => "ProverDisconnected",
            ServerMessage::ProverSubmit(_, _, _, _) => "ProverSubmit",
            ServerMessage::NewBlockTemplate(_) => "NewBlockTemplate",
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
    operator_sender: Sender<OperatorMessage>,
    accounting_sender: Sender<AccountingMessage>,
    connected_provers: RwLock<HashSet<SocketAddr>>,
    authenticated_provers: Arc<RwLock<HashMap<SocketAddr, Sender<ProverMessage>>>>,
    pool_state: Arc<RwLock<PoolState>>,
    prover_states: Arc<RwLock<HashMap<SocketAddr, RwLock<ProverState>>>>,
    prover_address_connections: Arc<RwLock<HashMap<Address<Testnet2>, HashSet<SocketAddr>>>>,
    latest_block_height: AtomicU32,
    latest_block_template: Arc<RwLock<Option<BlockTemplate<Testnet2>>>>,
}

impl Server {
    pub async fn init(
        port: u16,
        operator_sender: Sender<OperatorMessage>,
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

        let server = Arc::new(Server {
            sender,
            operator_sender,
            accounting_sender,
            connected_provers: Default::default(),
            authenticated_provers: Default::default(),
            pool_state: Arc::new(RwLock::new(PoolState::new())),
            prover_states: Default::default(),
            prover_address_connections: Default::default(),
            latest_block_height: AtomicU32::new(0),
            latest_block_template: Default::default(),
        });

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

        server
    }

    pub fn sender(&self) -> Sender<ServerMessage> {
        self.sender.clone()
    }

    pub async fn process_message(&self, msg: ServerMessage) {
        trace!("Received message: {}", msg);
        match msg {
            ServerMessage::ProverConnected(stream, peer_addr) => {
                self.connected_provers.write().await.insert(peer_addr);
                Connection::init(stream, peer_addr, self.sender.clone()).await;
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
                if let Some(block_template) = self.latest_block_template.read().await.clone() {
                    if let Err(e) = sender
                        .send(ProverMessage::Notify(block_template.clone(), u64::MAX))
                        .await
                    {
                        error!(
                            "Error sending initial block template to prover {} ({}): {}",
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
            ServerMessage::NewBlockTemplate(block_template) => {
                info!("New block template: {}", block_template.block_height());
                self.latest_block_height
                    .store(block_template.block_height(), Ordering::SeqCst);
                self.latest_block_template.write().await.replace(block_template.clone());
                if let Err(e) = self
                    .accounting_sender
                    .send(AccountingMessage::SetN(
                        u64::MAX / block_template.difficulty_target() * 5,
                    ))
                    .await
                {
                    error!("Error sending accounting message: {}", e);
                }
                let global_difficulty_modifier = self.pool_state.write().await.next_global_difficulty_modifier().await;
                debug!("Global difficulty modifier: {}", global_difficulty_modifier);
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
                    let difficulty =
                        (prover_state.write().await.next_difficulty().await as f64 * global_difficulty_modifier) as u64;
                    drop(states);
                    if let Err(e) = sender
                        .send(ProverMessage::Notify(block_template.clone(), u64::MAX / difficulty))
                        .await
                    {
                        error!("Error sending block template to prover {}: {}", prover_display, e);
                    }
                }
            }
            ServerMessage::ProverSubmit(peer_addr, block_height, nonce, proof) => {
                let prover_states = self.prover_states.clone();
                let pool_state = self.pool_state.clone();
                let authenticated_provers = self.authenticated_provers.clone();
                let latest_block_height = self.latest_block_height.load(Ordering::SeqCst);
                let current_global_difficulty_modifier =
                    self.pool_state.read().await.current_global_difficulty_modifier();
                let latest_block_template = self.latest_block_template.clone();
                let accounting_sender = self.accounting_sender.clone();
                let operator_sender = self.operator_sender.clone();
                task::spawn(async move {
                    async fn send_result(sender: &Sender<ProverMessage>, result: bool, desc: Option<String>) {
                        if let Err(e) = sender.send(ProverMessage::SubmitResult(result, desc)).await {
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
                            send_result(sender, false, Some("Unknown prover".to_string())).await;
                            return;
                        }
                    };
                    let prover_display = format!("{}", prover_state.read().await);
                    let block_template = match latest_block_template.read().await.clone() {
                        Some(template) => template,
                        None => {
                            warn!(
                                "Received proof from prover {} while no block template is available",
                                prover_display
                            );
                            send_result(sender, false, Some("No block template".to_string())).await;
                            return;
                        }
                    };
                    if block_height != latest_block_height {
                        info!(
                            "Received stale proof from prover {} with block height: {} (expected {})",
                            prover_display, block_height, latest_block_height
                        );
                        send_result(sender, false, Some("Stale proof".to_string())).await;
                        return;
                    }
                    if prover_state.write().await.seen_nonce(nonce) {
                        warn!("Received duplicate nonce from prover {}", prover_display);
                        send_result(sender, false, Some("Duplicate nonce".to_string())).await;
                        return;
                    }
                    let difficulty = (prover_state.read().await.current_difficulty() as f64
                        * current_global_difficulty_modifier) as u64;
                    let difficulty_target = u64::MAX / difficulty;
                    let proof_difficulty = match proof.to_proof_difficulty() {
                        Ok(difficulty) => difficulty,
                        Err(e) => {
                            warn!("Received invalid proof from prover {}: {}", prover_display, e);
                            send_result(sender, false, Some("No difficulty".to_string())).await;
                            return;
                        }
                    };
                    if proof_difficulty > difficulty_target {
                        warn!(
                            "Received proof with difficulty {} from prover {} (expected 8{})",
                            proof_difficulty, prover_display, difficulty_target
                        );
                        send_result(sender, false, Some("Difficulty target not met".to_string())).await;
                        return;
                    }
                    if !Testnet2::posw().verify(
                        block_height,
                        difficulty_target,
                        &[*block_template.to_header_root().unwrap(), *nonce],
                        &proof,
                    ) {
                        warn!("Received invalid proof from prover {}", prover_display);
                        send_result(sender, false, Some("Invalid proof".to_string())).await;
                        return;
                    }
                    prover_state.write().await.add_share(difficulty).await;
                    pool_state.write().await.add_share(difficulty).await;
                    if let Err(e) = accounting_sender
                        .send(AccountingMessage::NewShare(
                            prover_state.read().await.address().to_string(),
                            difficulty,
                        ))
                        .await
                    {
                        error!("Failed to send accounting message: {}", e);
                    }
                    send_result(sender, true, None).await;
                    drop(provers);
                    drop(states);
                    info!(
                        "Received valid proof from prover {} with difficulty {}",
                        prover_display, difficulty
                    );
                    if proof_difficulty <= block_template.difficulty_target() {
                        info!(
                            "Received unconfirmed block from prover {} with difficulty {} (target {})",
                            prover_display,
                            proof_difficulty,
                            block_template.difficulty_target()
                        );
                        if let Err(e) = operator_sender
                            .send(OperatorMessage::PoolBlock(nonce, Data::Object(proof)))
                            .await
                        {
                            error!("Failed to report unconfirmed block to operator: {}", e);
                        }
                        let reward = block_template.coinbase_record().value();
                        match block_template.to_header_root() {
                            Ok(header_root) => match &to_bytes_le![block_template.previous_block_hash(), header_root] {
                                Ok(block_hash_bytes) => match Testnet2::block_hash_crh().hash(block_hash_bytes) {
                                    Ok(block_hash) => {
                                        if let Err(e) = {
                                            accounting_sender
                                                .send(AccountingMessage::NewBlock(
                                                    block_height,
                                                    block_hash.into(),
                                                    reward,
                                                ))
                                                .await
                                        } {
                                            error!("Failed to send accounting message: {}", e);
                                        }
                                    }
                                    Err(e) => {
                                        error!("Failed to calculate block hash: {}", e);
                                    }
                                },
                                Err(e) => error!("Failed to convert header root to bytes: {}", e),
                            },
                            Err(e) => {
                                error!("Failed to get header root: {}", e);
                            }
                        }
                    }
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

    pub async fn address_prover_count(&self, address: Address<Testnet2>) -> u32 {
        self.prover_address_connections
            .read()
            .await
            .get(&address)
            .map(|prover_connections| prover_connections.len() as u32)
            .unwrap_or(0)
    }

    pub async fn address_speed(&self, address: Address<Testnet2>) -> Vec<f64> {
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
