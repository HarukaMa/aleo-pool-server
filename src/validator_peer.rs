use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use futures_util::sink::SinkExt;
use snarkos_node_executor::{NodeType, Status};
use snarkos_node_messages::{
    ChallengeRequest,
    ChallengeResponse,
    Data,
    MessageCodec,
    Ping,
    Pong,
    PuzzleRequest,
    PuzzleResponse,
};
use snarkvm::{
    prelude::{FromBytes, Network, Testnet3},
    synthesizer::Block,
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
    sender: Arc<Sender<SnarkOSMessage>>,
    receiver: Arc<Mutex<Receiver<SnarkOSMessage>>>,
}

pub(crate) type SnarkOSMessage = snarkos_node_messages::Message<Testnet3>;

impl Node {
    pub fn init(operator: String) -> Self {
        let (sender, receiver) = mpsc::channel(1024);
        Self {
            operator,
            sender: Arc::new(sender),
            receiver: Arc::new(Mutex::new(receiver)),
        }
    }

    pub fn receiver(&self) -> Arc<Mutex<Receiver<SnarkOSMessage>>> {
        self.receiver.clone()
    }

    pub fn sender(&self) -> Arc<Sender<SnarkOSMessage>> {
        self.sender.clone()
    }
}

pub fn start(node: Node, server_sender: Sender<ServerMessage>) {
    let receiver = node.receiver();
    let sender = node.sender();
    task::spawn(async move {
        let genesis_header = *Block::<Testnet3>::from_bytes_le(Testnet3::genesis_bytes())
            .unwrap()
            .header();
        let connected = Arc::new(AtomicBool::new(false));
        let peer_sender = sender.clone();

        let connected_req = connected.clone();
        task::spawn(async move {
            loop {
                sleep(Duration::from_secs(Testnet3::ANCHOR_TIME as u64)).await;
                if connected_req.load(Ordering::SeqCst) {
                    if let Err(e) = peer_sender.send(SnarkOSMessage::PuzzleRequest(PuzzleRequest {})).await {
                        error!("Failed to send puzzle request: {}", e);
                    }
                }
            }
        });
        loop {
            info!("Connecting to operator...");
            match timeout(Duration::from_secs(5), TcpStream::connect(&node.operator)).await {
                Ok(socket) => match socket {
                    Ok(socket) => {
                        info!("Connected to {}", node.operator);
                        let mut framed: Framed<TcpStream, MessageCodec<Testnet3>> =
                            Framed::new(socket, Default::default());
                        let challenge = SnarkOSMessage::ChallengeRequest(ChallengeRequest {
                            version: SnarkOSMessage::VERSION,
                            fork_depth: 4096,
                            node_type: NodeType::Prover,
                            status: Status::Ready,
                            listener_port: 4140,
                        });
                        if let Err(e) = framed.send(challenge).await {
                            error!("Error sending challenge request: {}", e);
                        } else {
                            trace!("Sent challenge request");
                        }
                        let receiver = &mut *receiver.lock().await;
                        loop {
                            tokio::select! {
                                Some(message) = receiver.recv() => {
                                    trace!("Sending {} to validator", message.name());
                                    if let Err(e) = framed.send(message.clone()).await {
                                        error!("Error sending {}: {:?}", message.name(), e);
                                    }
                                }
                                result = framed.next() => match result {
                                    Some(Ok(message)) => {
                                        trace!("Received {} from operator", message.name());
                                        match message {
                                            SnarkOSMessage::ChallengeRequest(ChallengeRequest {
                                                version,
                                                fork_depth: _,
                                                node_type,
                                                ..
                                            }) => {
                                                if version < SnarkOSMessage::VERSION {
                                                    error!("Peer is running an older version of the protocol");
                                                    sleep(Duration::from_secs(5)).await;
                                                    break;
                                                }
                                                if node_type != NodeType::Beacon && node_type != NodeType::Validator {
                                                    error!("Peer is not a beacon or validator");
                                                    sleep(Duration::from_secs(5)).await;
                                                    break;
                                                }
                                                let response = SnarkOSMessage::ChallengeResponse(ChallengeResponse {
                                                    header: Data::Object(genesis_header),
                                                });
                                                if let Err(e) = framed.send(response).await {
                                                    error!("Error sending challenge response: {:?}", e);
                                                } else {
                                                    debug!("Sent challenge response");
                                                }
                                            }
                                            SnarkOSMessage::ChallengeResponse(message) => {
                                                let block_header = match message.header.deserialize().await {
                                                    Ok(block_header) => block_header,
                                                    Err(error) => {
                                                        error!("Error deserializing block header: {:?}", error);
                                                        sleep(Duration::from_secs(5)).await;
                                                        break;
                                                    }
                                                };
                                                match block_header == genesis_header {
                                                    true => {
                                                        // Send the first `Ping` message to the peer.
                                                        let message = SnarkOSMessage::Ping(Ping {
                                                            version: SnarkOSMessage::VERSION,
                                                            fork_depth: 4096,
                                                            node_type: NodeType::Prover,
                                                            status: Status::Ready,
                                                        });
                                                        if let Err(e) = framed.send(message).await {
                                                            error!("Error sending ping: {:?}", e);
                                                        } else {
                                                            debug!("Sent ping");
                                                        }
                                                    }
                                                    false => {
                                                        error!("Peer has a different genesis block");
                                                        sleep(Duration::from_secs(5)).await;
                                                        break;
                                                    }
                                                }
                                            }
                                            SnarkOSMessage::Ping(..) => {
                                                let pong = SnarkOSMessage::Pong(Pong { is_fork: None });
                                                if let Err(e) = framed.send(pong).await {
                                                    error!("Error sending pong: {:?}", e);
                                                } else {
                                                    debug!("Sent pong");
                                                }
                                            }
                                            SnarkOSMessage::Pong(..) => {
                                                connected.store(true, Ordering::SeqCst);
                                                if let Err(e) = sender.send(SnarkOSMessage::PuzzleRequest(PuzzleRequest {})).await {
                                                    error!("Failed to send puzzle request: {}", e);
                                                }
                                            }
                                            SnarkOSMessage::PuzzleResponse(PuzzleResponse {
                                                epoch_challenge, block
                                            }) => {
                                                let block = match block.deserialize().await {
                                                    Ok(block) => block,
                                                    Err(error) => {
                                                        error!("Error deserializing block: {:?}", error);
                                                        sleep(Duration::from_secs(5)).await;
                                                        break;
                                                    }
                                                };
                                                let epoch_number = epoch_challenge.epoch_number();
                                                if let Err(e) = server_sender.send(ServerMessage::NewEpochChallenge(
                                                    epoch_challenge, block.coinbase_target(), block.proof_target()
                                                )).await {
                                                    error!("Error sending new block template to pool server: {}", e);
                                                } else {
                                                    trace!("Sent new epoch challenge {} to pool server", epoch_number);
                                                }
                                            }
                                            SnarkOSMessage::Disconnect(message) => {
                                                error!("Peer disconnected: {:?}", message.reason);
                                                sleep(Duration::from_secs(5)).await;
                                                break;
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
