use futures_util::sink::SinkExt;
use rand::thread_rng;
use rand::Rng;
use snarkos::environment::Prover;
use snarkos::helpers::{NodeType, State};
use snarkos::{Data, Environment, Message, OperatorTrial};
use snarkos_storage::BlockLocators;
use snarkvm::dpc::testnet2::Testnet2;
use snarkvm::dpc::{Address, BlockHeader};
use snarkvm::traits::Network;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::{mpsc, Mutex};
use tokio::task;
use tokio::time::{sleep, timeout};
use tokio_stream::StreamExt;
use tokio_util::codec::Framed;
use tracing::{debug, error, info, warn};

pub struct Node {
    operator: String,
    router: Arc<Sender<OperatorMessage>>,
    receiver: Arc<Mutex<Receiver<OperatorMessage>>>,
}

#[derive(Debug)]
pub struct OperatorMessage {
    pub(crate) message: Message<Testnet2, OperatorTrial<Testnet2>>,
}

impl Node {
    pub fn init(operator: String) -> Arc<Self> {
        let (router_tx, router_rx) = mpsc::channel(1024);
        Arc::new(Self {
            operator,
            router: Arc::new(router_tx),
            receiver: Arc::new(Mutex::new(router_rx)),
        })
    }

    pub fn router(&self) -> Arc<Sender<OperatorMessage>> {
        self.router.clone()
    }

    pub fn receiver(&self) -> Arc<Mutex<Receiver<OperatorMessage>>> {
        self.receiver.clone()
    }
}

pub fn start(
    node: Arc<Node>,
    receiver: Arc<Mutex<Receiver<OperatorMessage>>>,
) {
    task::spawn(async move {
        loop {
            info!("Connecting to operator...");
            match timeout(Duration::from_secs(5), TcpStream::connect(&node.operator)).await {
                Ok(socket) => match socket {
                    Ok(socket) => {
                        info!("Connected to {}", node.operator);
                        let mut framed =
                            Framed::new(socket, Message::<Testnet2, OperatorTrial<Testnet2>>::PeerRequest);
                        let challenge = Message::ChallengeRequest(
                            OperatorTrial::<Testnet2>::MESSAGE_VERSION,
                            Testnet2::ALEO_MAXIMUM_FORK_DEPTH,
                            NodeType::PoolServer,
                            State::Ready,
                            4132,
                            thread_rng().gen(),
                            0,
                        );
                        if let Err(e) = framed.send(challenge).await {
                            error!("Error sending challenge request: {}", e);
                        } else {
                            debug!("Sent challenge request");
                        }
                        let receiver = &mut *receiver.lock().await;
                        loop {
                            tokio::select! {
                                Some(message) = receiver.recv() => {
                                    let message = message.message.clone();
                                    debug!("Sending {} to operator", message.name());
                                    if let Err(e) = framed.send(message.clone()).await {
                                        error!("Error sending {}: {:?}", message.name(), e);
                                    }
                                }
                                result = framed.next() => match result {
                                    Some(Ok(message)) => {
                                        debug!("Received {} from operator", message.name());
                                        match message {
                                            Message::ChallengeRequest(..) => {
                                                let resp = Message::ChallengeResponse(Data::Object(Testnet2::genesis_block().header().clone()));
                                                if let Err(e) = framed.send(resp).await {
                                                    error!("Error sending challenge response: {:?}", e);
                                                } else {
                                                    debug!("Sent challenge response");
                                                }
                                            }
                                            Message::ChallengeResponse(..) => {
                                                let ping = Message::<Testnet2, OperatorTrial<Testnet2>>::Ping(
                                                    12,
                                                    Testnet2::ALEO_MAXIMUM_FORK_DEPTH,
                                                    NodeType::PoolServer,
                                                    State::Ready,
                                                    Testnet2::genesis_block().hash(),
                                                    Data::Object(Testnet2::genesis_block().header().clone()),
                                                );
                                                if let Err(e) = framed.send(ping).await {
                                                    error!("Error sending ping: {:?}", e);
                                                } else {
                                                    debug!("Sent ping");
                                                }
                                            }
                                            Message::Ping(..) => {
                                                let mut locators: BTreeMap<u32, (<Testnet2 as Network>::BlockHash, Option<BlockHeader<Testnet2>>)> = BTreeMap::new();
                                                locators.insert(0, (Testnet2::genesis_block().hash(), None));
                                                let resp = Message::<Testnet2, OperatorTrial<Testnet2>>::Pong(None, Data::Object(BlockLocators::<Testnet2>::from(locators).unwrap_or_default()));
                                                if let Err(e) = framed.send(resp).await {
                                                    error!("Error sending pong: {:?}", e);
                                                } else {
                                                    debug!("Sent pong");
                                                }
                                            }
                                            Message::Pong(..) => {
                                            }
                                            Message::UnconfirmedBlock(..) => {}
                                            Message::NewBlockTemplate(template) => {
                                                if let Ok(template) = template.deserialize().await {
                                                    error!("Received new block template: {}", template.block_height());
                                                } else {
                                                    error!("Error deserializing block template");
                                                }
                                            }
                                            _ => {
                                                debug!("Unhandled message: {}", message.name());
                                            }
                                        }
                                    }
                                    Some(Err(e)) => {
                                        warn!("Failed to read the message: {:?}", e);
                                    }
                                    None => {
                                        error!("Disconnected from operator");
                                        sleep(Duration::from_secs(5)).await;
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!("Failed to connect to operator: {}", e);
                        sleep(Duration::from_secs(5)).await;
                    }
                },
                Err(_) => {
                    error!("Failed to connect to operator: Timed out");
                    sleep(Duration::from_secs(5)).await;
                }
            }
        }
    });
}
