use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use futures_util::sink::SinkExt;
use rand::{rngs::OsRng, Rng};
use snarkos_account::Account;
use snarkos_node_messages::{
    ChallengeRequest,
    ChallengeResponse,
    Data,
    MessageCodec,
    NodeType,
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
        let peer_sender_ping = sender.clone();

        let connected_req = connected.clone();
        let connected_ping = connected.clone();
        task::spawn(async move {
            loop {
                sleep(Duration::from_secs(15)).await;
                if connected_req.load(Ordering::SeqCst) {
                    if let Err(e) = peer_sender.send(SnarkOSMessage::PuzzleRequest(PuzzleRequest {})).await {
                        error!("Failed to send puzzle request: {}", e);
                    }
                }
            }
        });
        task::spawn(async move {
            loop {
                sleep(Duration::from_secs(5)).await;
                if connected_ping.load(Ordering::SeqCst) {
                    if let Err(e) = peer_sender_ping
                        .send(SnarkOSMessage::Ping(Ping {
                            version: SnarkOSMessage::VERSION,
                            node_type: NodeType::Prover,
                            block_locators: None,
                        }))
                        .await
                    {
                        error!("Failed to send ping: {}", e);
                    }
                }
            }
        });

        let rng = &mut OsRng;
        let random_account = Account::new(rng).unwrap();
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
                            listener_port: 4140,
                            node_type: NodeType::Prover,
                            address: random_account.address(),
                            nonce: rng.gen(),
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
                                        trace!("Received {} from validator", message.name());
                                        match message {
                                            SnarkOSMessage::ChallengeRequest(ChallengeRequest {
                                                version,
                                                listener_port: _,
                                                node_type,
                                                address: _,
                                                nonce,
                                            }) => {
                                                if version < SnarkOSMessage::VERSION {
                                                    error!("Peer is running an older version of the protocol");
                                                    sleep(Duration::from_secs(25)).await;
                                                    break;
                                                }
                                                if node_type != NodeType::Beacon && node_type != NodeType::Validator {
                                                    error!("Peer is not a beacon or validator");
                                                    sleep(Duration::from_secs(25)).await;
                                                    break;
                                                }
                                                let response = SnarkOSMessage::ChallengeResponse(ChallengeResponse {
                                                    genesis_header,
                                                    signature: Data::Object(random_account.sign_bytes(&nonce.to_le_bytes(), rng).unwrap()),
                                                });
                                                if let Err(e) = framed.send(response).await {
                                                    error!("Error sending challenge response: {:?}", e);
                                                } else {
                                                    debug!("Sent challenge response");
                                                }
                                            }
                                            SnarkOSMessage::ChallengeResponse(message) => {
                                                match message.genesis_header == genesis_header {
                                                    true => {
                                                        let was_connected = connected.load(Ordering::SeqCst);
                                                        connected.store(true, Ordering::SeqCst);
                                                        if !was_connected {
                                                            if let Err(e) = sender.send(SnarkOSMessage::PuzzleRequest(PuzzleRequest {})).await {
                                                                error!("Failed to send puzzle request: {}", e);
                                                            }
                                                        }
                                                    }
                                                    false => {
                                                        error!("Peer has a different genesis block");
                                                        sleep(Duration::from_secs(25)).await;
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
                                                let message = SnarkOSMessage::Ping(Ping {
                                                    version: SnarkOSMessage::VERSION,
                                                    node_type: NodeType::Prover,
                                                    block_locators: None,
                                                });
                                                if let Err(e) = framed.send(message).await {
                                                    error!("Error sending ping: {:?}", e);
                                                } else {
                                                    debug!("Sent ping");
                                                }
                                            }
                                            SnarkOSMessage::PuzzleResponse(PuzzleResponse {
                                                epoch_challenge, block_header
                                            }) => {
                                                let block_header = match block_header.deserialize().await {
                                                    Ok(block_header) => block_header,
                                                    Err(error) => {
                                                        error!("Error deserializing block header: {:?}", error);
                                                        connected.store(false, Ordering::SeqCst);
                                                        sleep(Duration::from_secs(25)).await;
                                                        break;
                                                    }
                                                };
                                                let epoch_number = epoch_challenge.epoch_number();
                                                if let Err(e) = server_sender.send(ServerMessage::NewEpochChallenge(
                                                    epoch_challenge, block_header.proof_target()
                                                )).await {
                                                    error!("Error sending new block template to pool server: {}", e);
                                                } else {
                                                    trace!("Sent new epoch challenge {} to pool server", epoch_number);
                                                }
                                            }
                                            SnarkOSMessage::Disconnect(message) => {
                                                error!("Peer disconnected: {:?}", message.reason);
                                                connected.store(false, Ordering::SeqCst);
                                                sleep(Duration::from_secs(25)).await;
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
                                        connected.store(false, Ordering::SeqCst);
                                        sleep(Duration::from_secs(25)).await;
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!("Failed to connect to operator: {}", e);
                        sleep(Duration::from_secs(25)).await;
                    }
                },
                Err(_) => {
                    error!("Failed to connect to operator: Timed out");
                    sleep(Duration::from_secs(25)).await;
                }
            }
        }
    });
}
