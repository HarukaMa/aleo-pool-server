use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use snarkvm::dpc::BlockTemplate;
use snarkvm::dpc::testnet2::Testnet2;
use tokio::net::TcpListener;
use tokio::sync::mpsc::{channel, Sender};
use tokio::task;
use tracing::{error, info};
use crate::operator_peer::OperatorMessage;

#[derive(Debug)]
pub enum ServerMessage {
    ProverConnected(SocketAddr),
    ProverDisconnected(SocketAddr),
    NewBlockTemplate(BlockTemplate<Testnet2>),

}

pub struct Server {
    port: u16,
    router: Sender<ServerMessage>,
    operator_router: Arc<Sender<OperatorMessage>>,
}

impl Server {

    pub async fn init(port: u16, operator_router: Arc<Sender<OperatorMessage>>) -> Arc<Server> {
        let (sender, mut receiver) = channel(1024);

        let (_, listener) = match TcpListener::bind(format!("0.0.0.0:{}", port)).await {
            Ok(listener) => {
                let local_ip = listener.local_addr().expect("Could not get local ip");
                info!("Listening on {}", local_ip);
                (local_ip, listener)
            },
            Err(e) => {
                panic!("Unable to start the server: {:?}", e);
            }
        };

        let server = Arc::new(Server {
            port,
            router: sender,
            operator_router,
        });

        task::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, peer_addr)) => {
                        info!("New connection from: {}", peer_addr);

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

    pub async fn process_message(&self, msg: ServerMessage) {
        info!("Received message: {:?}", msg);
        match msg {
            ServerMessage::ProverConnected(peer_addr) => {
            }
            ServerMessage::ProverDisconnected(_) => {}
            ServerMessage::NewBlockTemplate(_) => {}
        }
    }

}