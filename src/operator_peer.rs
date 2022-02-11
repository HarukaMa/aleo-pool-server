use std::{collections::BTreeMap, sync::Arc, time::Duration};

use futures_util::sink::SinkExt;
use rand::{thread_rng, Rng};
use snarkos::{
    helpers::{NodeType, State},
    Data,
    Environment,
    Message,
    OperatorTrial,
};
use snarkos_storage::BlockLocators;
use snarkvm::{
    dpc::{testnet2::Testnet2, BlockHeader},
    traits::Network,
};
use tokio::{
    net::TcpStream,
    sync::{
        mpsc,
        mpsc::{Receiver, Sender},
        Mutex,
    },
    task,
    time::{sleep, timeout},
};
use tokio_stream::StreamExt;
use tokio_util::codec::Framed;
use tracing::{debug, error, info, trace, warn};

use crate::ServerMessage;

pub struct Node {
    operator: String,
    sender: Sender<OperatorMessage>,
    receiver: Arc<Mutex<Receiver<OperatorMessage>>>,
}

pub(crate) type OperatorMessage = Message<Testnet2, OperatorTrial<Testnet2>>;

impl Node {
    pub fn init(operator: String) -> Self {
        let (sender, receiver) = mpsc::channel(1024);
        Self {
            operator,
            sender,
            receiver: Arc::new(Mutex::new(receiver)),
        }
    }

    pub fn sender(&self) -> Sender<OperatorMessage> {
        self.sender.clone()
    }
}

pub fn start(node: Node, server_sender: Sender<ServerMessage>) {
    let receiver = node.receiver;
    task::spawn(async move {
        loop {
            info!("Connecting to operator...");
            match timeout(Duration::from_secs(5), TcpStream::connect(&node.operator)).await {
                Ok(socket) => match socket {
                    Ok(socket) => {
                        info!("Connected to {}", node.operator);
                        let mut framed = Framed::new(socket, Message::PeerRequest);
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
                            trace!("Sent challenge request");
                        }
                        let receiver = &mut *receiver.lock().await;
                        loop {
                            tokio::select! {
                                Some(message) = receiver.recv() => {
                                    trace!("Sending {} to operator", message.name());
                                    if let Err(e) = framed.send(message.clone()).await {
                                        error!("Error sending {}: {:?}", message.name(), e);
                                    }
                                }
                                result = framed.next() => match result {
                                    Some(Ok(message)) => {
                                        trace!("Received {} from operator", message.name());
                                        match message {
                                            Message::ChallengeRequest(..) => {
                                                let resp = Message::ChallengeResponse(Data::Object(Testnet2::genesis_block().header().clone()));
                                                if let Err(e) = framed.send(resp).await {
                                                    error!("Error sending challenge response: {:?}", e);
                                                } else {
                                                    trace!("Sent challenge response");
                                                }
                                            }
                                            Message::ChallengeResponse(..) => {
                                                let ping = Message::Ping(
                                                    OperatorTrial::<Testnet2>::MESSAGE_VERSION,
                                                    Testnet2::ALEO_MAXIMUM_FORK_DEPTH,
                                                    NodeType::PoolServer,
                                                    State::Ready,
                                                    Testnet2::genesis_block().hash(),
                                                    Data::Object(Testnet2::genesis_block().header().clone()),
                                                );
                                                if let Err(e) = framed.send(ping).await {
                                                    error!("Error sending ping: {:?}", e);
                                                } else {
                                                    trace!("Sent ping");
                                                }
                                            }
                                            Message::Ping(..) => {
                                                let mut locators: BTreeMap<u32, (<Testnet2 as Network>::BlockHash, Option<BlockHeader<Testnet2>>)> = BTreeMap::new();
                                                locators.insert(0, (Testnet2::genesis_block().hash(), None));
                                                let resp = Message::Pong(None, Data::Object(BlockLocators::<Testnet2>::from(locators).unwrap_or_default()));
                                                if let Err(e) = framed.send(resp).await {
                                                    error!("Error sending pong: {:?}", e);
                                                } else {
                                                    trace!("Sent pong");
                                                }
                                            }
                                            Message::Pong(..) => {
                                            }
                                            Message::UnconfirmedBlock(..) => {}
                                            Message::NewBlockTemplate(template) => {
                                                if let Ok(template) = template.deserialize().await {
                                                    let block_height = template.block_height();
                                                    if let Err(e) = server_sender.send(ServerMessage::NewBlockTemplate(template)).await {
                                                        error!("Error sending new block template to pool server: {}", e);
                                                    } else {
                                                        trace!("Sent new block template {} to pool server", block_height);
                                                    }
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
