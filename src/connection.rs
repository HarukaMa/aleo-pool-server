use std::{
    net::SocketAddr,
    str::FromStr,
    time::{Duration, Instant},
};

use aleo_stratum::{
    codec::{BoxedType, ResponseParams, StratumCodec},
    message::StratumMessage,
};
use anyhow::{anyhow, Result};
use futures_util::SinkExt;
use semver::Version;
use snarkvm::{
    console::account::address::Address,
    prelude::{Environment, FromBytes, Testnet3},
};
use snarkvm_algorithms::polycommit::kzg10::{KZGCommitment, KZGProof};
use tokio::{
    net::TcpStream,
    sync::mpsc::{channel, Sender},
    task,
    time::timeout,
};
use tokio_stream::StreamExt;
use tokio_util::codec::Framed;
use tracing::{error, info, trace, warn};

use crate::server::ServerMessage;

pub struct Connection {
    user_agent: String,
    address: Option<Address<Testnet3>>,
    version: Version,
    last_received: Option<Instant>,
}

static PEER_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
static PEER_COMM_TIMEOUT: Duration = Duration::from_secs(180);

static MIN_SUPPORTED_VERSION: Version = Version::new(2, 0, 0);
static MAX_SUPPORTED_VERSION: Version = Version::new(2, 0, 0);

impl Connection {
    pub async fn init(
        stream: TcpStream,
        peer_addr: SocketAddr,
        server_sender: Sender<ServerMessage>,
        pool_address: Address<Testnet3>,
    ) {
        task::spawn(Connection::run(stream, peer_addr, server_sender, pool_address));
    }

    pub async fn run(
        stream: TcpStream,
        peer_addr: SocketAddr,
        server_sender: Sender<ServerMessage>,
        pool_address: Address<Testnet3>,
    ) {
        let mut framed = Framed::new(stream, StratumCodec::default());

        let (sender, mut receiver) = channel(1024);

        let mut conn = Connection {
            user_agent: "Unknown".to_string(),
            address: None,
            version: Version::new(0, 0, 0),
            last_received: None,
        };

        // Handshake

        if let Ok((user_agent, version)) = Connection::handshake(&mut framed, pool_address.to_string()).await {
            conn.user_agent = user_agent;
            conn.version = version;
        } else {
            if let Err(e) = server_sender.send(ServerMessage::ProverDisconnected(peer_addr)).await {
                error!("Failed to send ProverDisconnected message to server: {}", e);
            }
            return;
        }

        if let Ok(address) = Connection::authorize(&mut framed).await {
            conn.address = Some(address);
            if let Err(e) = server_sender
                .send(ServerMessage::ProverAuthenticated(
                    peer_addr,
                    conn.address.unwrap(),
                    sender,
                ))
                .await
            {
                error!("Failed to send ProverAuthenticated message to server: {}", e);
            }
        } else {
            if let Err(e) = server_sender.send(ServerMessage::ProverDisconnected(peer_addr)).await {
                error!("Failed to send ProverDisconnected message to server: {}", e);
            }
            return;
        }

        conn.last_received = Some(Instant::now());

        info!("Peer {:?} authenticated as {}", peer_addr, conn.address.unwrap());

        loop {
            tokio::select! {
                Some(msg) = receiver.recv() => {
                    if let Some(instant) = conn.last_received {
                        if instant.elapsed() > PEER_COMM_TIMEOUT {
                            warn!("Peer {:?} timed out", peer_addr);
                            break;
                        }
                    }
                    trace!("Sending message {} to peer {:?}", msg.name(), peer_addr);
                    if let Err(e) = framed.send(msg).await {
                        error!("Failed to send message to peer {:?}: {:?}", peer_addr, e);
                    }
                },
                result = framed.next() => match result {
                    Some(Ok(msg)) => {
                        trace!("Received message {} from peer {:?}", msg.name(), peer_addr);
                        conn.last_received = Some(Instant::now());
                        match msg {
                            StratumMessage::Submit(id, _worker_name, job_id, nonce, commitment, proof) => {
                                let job_bytes = hex::decode(job_id.clone());
                                if job_bytes.is_err() {
                                    warn!("Failed to decode job_id {} from peer {:?}", job_id, peer_addr);
                                    break;
                                }
                                if job_bytes.clone().unwrap().len() != 4 {
                                    warn!("Invalid job_id {} from peer {:?}", job_id, peer_addr);
                                    break;
                                }
                                let epoch_number = u32::from_le_bytes(job_bytes.unwrap().try_into().unwrap());
                                let nonce_bytes = hex::decode(nonce.clone());
                                if nonce_bytes.is_err() {
                                    warn!("Failed to decode nonce {} from peer {:?}", nonce, peer_addr);
                                    break;
                                }
                                let nonce = u64::from_le_bytes(nonce_bytes.unwrap().try_into().unwrap());
                                let commitment_bytes = hex::decode(commitment.clone());
                                if commitment_bytes.is_err() {
                                    warn!("Failed to decode commitment {} from peer {:?}", commitment, peer_addr);
                                    break;
                                }
                                let commitment = KZGCommitment::<<Testnet3 as Environment>::PairingCurve>::from_bytes_le(&commitment_bytes.unwrap()[..]);
                                if commitment.is_err() {
                                    warn!("Invalid commitment from peer {:?}", peer_addr);
                                    break;
                                }
                                let proof_bytes = hex::decode(proof.clone());
                                if proof_bytes.is_err() {
                                warn!("Failed to decode proof {} from peer {:?}", proof, peer_addr);
                                    break;
                                }
                                let proof = KZGProof::<<Testnet3 as Environment>::PairingCurve>::from_bytes_le(&proof_bytes.unwrap());
                                if proof.is_err() {
                                    warn!("Invalid proof from peer {:?}", peer_addr);
                                    break;
                                }
                                if let Err(e) = server_sender.send(ServerMessage::ProverSubmit(id, peer_addr, epoch_number, nonce, commitment.unwrap(), proof.unwrap())).await {
                                    error!("Failed to send ProverSubmit message to server: {}", e);
                                }
                            }
                            _ => {
                                warn!("Received unexpected message from peer {:?}: {:?}", peer_addr, msg.name());
                                break;
                            }
                        }
                    }
                    Some(Err(e)) => {
                        warn!("Failed to read message from peer: {:?}", e);
                        break;
                    }
                    None => {
                        info!("Peer {:?} disconnected", peer_addr);
                        break;
                    }
                },
                _ = tokio::time::sleep(PEER_COMM_TIMEOUT) => {
                    info!("Peer {:?} timed out", peer_addr);
                    break;
                },
            }
        }
        if let Err(e) = server_sender.send(ServerMessage::ProverDisconnected(peer_addr)).await {
            error!("Failed to send ProverDisconnected message to server: {}", e);
        }
    }

    pub async fn handshake(
        framed: &mut Framed<TcpStream, StratumCodec>,
        pool_address: String,
    ) -> Result<(String, Version)> {
        let peer_addr = framed.get_ref().peer_addr()?;
        match timeout(PEER_HANDSHAKE_TIMEOUT, framed.next()).await {
            Ok(Some(Ok(message))) => {
                trace!("Received message {} from peer {:?}", message.name(), peer_addr);
                match message {
                    StratumMessage::Subscribe(id, user_agent, protocol_version, _) => {
                        let split: Vec<&str> = protocol_version.split('/').collect();
                        if split.len() != 2 {
                            warn!(
                                "Invalid protocol version {} from peer {:?}",
                                protocol_version, peer_addr
                            );
                            return Err(anyhow!("Invalid protocol version"));
                        }
                        if split[0] != "AleoStratum" {
                            warn!("Invalid protocol name {} from peer {:?}", split[0], peer_addr);
                            return Err(anyhow!("Invalid protocol name"));
                        }
                        let version = Version::parse(split[1]).map_err(|e| {
                            warn!(
                                "Invalid protocol version {} from peer {:?}: {:?}",
                                split[1], peer_addr, e
                            );
                            e
                        })?;
                        if version < MIN_SUPPORTED_VERSION || version > MAX_SUPPORTED_VERSION {
                            warn!("Unsupported protocol version {} from peer {:?}", version, peer_addr);
                            return Err(anyhow!("Unsupported protocol version"));
                        }
                        let response_params: Vec<Box<dyn BoxedType>> = vec![
                            Box::new(Option::<String>::None),
                            Box::new(Option::<String>::None),
                            Box::new(Some(pool_address)),
                        ];
                        framed
                            .send(StratumMessage::Response(
                                id,
                                Some(ResponseParams::Array(response_params)),
                                None,
                            ))
                            .await?;
                        Ok((user_agent, version))
                    }
                    _ => {
                        warn!("Peer {:?} sent {} before handshake", peer_addr, message.name());
                        Err(anyhow!("Unexpected message before handshake"))
                    }
                }
            }
            Ok(Some(Err(e))) => {
                warn!("Error reading from peer {:?}: {}", peer_addr, e);
                Err(anyhow!("Error reading from peer"))
            }
            Ok(None) => {
                warn!("Peer {:?} disconnected before authorization", peer_addr);
                Err(anyhow!("Peer disconnected before authorization"))
            }
            Err(e) => {
                warn!("Peer {:?} timed out on handshake: {}", peer_addr, e);
                Err(anyhow!("Peer timed out on handshake"))
            }
        }
    }

    pub async fn authorize(framed: &mut Framed<TcpStream, StratumCodec>) -> Result<Address<Testnet3>> {
        let peer_addr = framed.get_ref().peer_addr()?;
        match timeout(PEER_HANDSHAKE_TIMEOUT, framed.next()).await {
            Ok(Some(Ok(message))) => {
                trace!("Received message {} from peer {:?}", message.name(), peer_addr);
                match message {
                    StratumMessage::Authorize(id, address, _) => {
                        let address = Address::<Testnet3>::from_str(address.as_str()).map_err(|e| {
                            warn!("Invalid address {} from peer {:?}: {:?}", address, peer_addr, e);
                            e
                        })?;
                        framed
                            .send(StratumMessage::Response(id, Some(ResponseParams::Bool(true)), None))
                            .await?;
                        Ok(address)
                    }
                    _ => {
                        warn!("Peer {:?} sent {} before authorizing", peer_addr, message.name());
                        Err(anyhow!("Unexpected message before authorization"))
                    }
                }
            }
            Ok(Some(Err(e))) => {
                warn!("Error reading from peer {:?}: {}", peer_addr, e);
                Err(anyhow!("Error reading from peer"))
            }
            Ok(None) => {
                warn!("Peer {:?} disconnected before authorization", peer_addr);
                Err(anyhow!("Peer disconnected before authorization"))
            }
            Err(e) => {
                warn!("Peer {:?} timed out on authorize: {}", peer_addr, e);
                Err(anyhow!("Peer timed out on authorize"))
            }
        }
    }
}
