use std::net::SocketAddr;

use anyhow::{anyhow, Result};
use futures_util::SinkExt;
use snarkvm::dpc::{testnet2::Testnet2, Address};
use tokio::{
    net::TcpStream,
    sync::mpsc::{channel, Sender},
    task,
};
use tokio_stream::StreamExt;
use tokio_util::codec::Framed;
use tracing::{error, info, trace, warn};

use crate::{message::ProverMessage, server::ServerMessage};

pub struct Connection {
    address: Option<Address<Testnet2>>,
    version: u16,
}

static MIN_SUPPORTED_VERSION: u16 = 1;
static MAX_SUPPORTED_VERSION: u16 = 1;

impl Connection {
    pub async fn init(stream: TcpStream, peer_addr: SocketAddr, server_sender: Sender<ServerMessage>) {
        task::spawn(Connection::run(stream, peer_addr, server_sender));
    }

    pub async fn run(stream: TcpStream, peer_addr: SocketAddr, server_sender: Sender<ServerMessage>) {
        let mut framed = Framed::new(stream, ProverMessage::Canary);

        let (sender, mut receiver) = channel(1024);

        let mut conn = Connection {
            address: None,
            version: 0,
        };

        if let Ok((address, version)) = Connection::authorize(&mut framed).await {
            conn.address = Some(address);
            conn.version = version;
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
        } else if let Err(e) = server_sender.send(ServerMessage::ProverDisconnected(peer_addr)).await {
            error!("Failed to send ProverDisconnected message to server: {}", e);
        }

        info!("Peer {:?} authenticated as {}", peer_addr, conn.address.unwrap());

        loop {
            tokio::select! {
                Some(msg) = receiver.recv() => {
                    trace!("Sending message {} to peer {:?}", msg.name(), peer_addr);
                    if let Err(e) = framed.send(msg).await {
                        error!("Failed to send message to peer {:?}: {:?}", peer_addr, e);
                    }
                }
                result = framed.next() => match result {
                    Some(Ok(msg)) => {
                        match msg {
                            ProverMessage::Submit(height, nonce, proof) => {
                                if let Err(e) = server_sender
                                    .send(ServerMessage::ProverSubmit(
                                        peer_addr,
                                        height,
                                        nonce,
                                        proof,
                                    ))
                                    .await
                                {
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
                }
            }
        }
        if let Err(e) = server_sender.send(ServerMessage::ProverDisconnected(peer_addr)).await {
            error!("Failed to send ProverDisconnected message to server: {}", e);
        }
    }

    pub async fn authorize(framed: &mut Framed<TcpStream, ProverMessage>) -> Result<(Address<Testnet2>, u16)> {
        let peer_addr = framed.get_ref().peer_addr()?;
        match framed.next().await {
            Some(Ok(message)) => {
                trace!("Received message {} from peer {:?}", message.name(), peer_addr);
                match message {
                    ProverMessage::Authorize(address, _, version) => {
                        if version > MAX_SUPPORTED_VERSION || version < MIN_SUPPORTED_VERSION {
                            warn!("Peer {:?} is using unsupported version {}", peer_addr, version);
                            return Err(anyhow!("Unsupported version"));
                        }
                        Ok((address, version))
                    }
                    _ => {
                        warn!("Peer {:?} sent {} before authorizing", peer_addr, message.name());
                        return Err(anyhow!("Unexpected message before authorization"));
                    }
                }
            }
            Some(Err(e)) => {
                warn!("Error reading from peer {:?}: {}", peer_addr, e);
                return Err(anyhow!("Error reading from peer"));
            }
            None => {
                warn!("Peer {:?} disconnected before authorization", peer_addr);
                return Err(anyhow!("Peer disconnected before authorization"));
            }
        }
    }
}
