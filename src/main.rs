mod accounting;
mod api;
mod connection;
mod server;
mod validator_peer;

#[cfg(feature = "db")]
mod db;

use std::sync::Arc;

use clap::Parser;
use futures::stream::StreamExt;
use rand::seq::SliceRandom;
use signal_hook::consts::{SIGABRT, SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGTSTP, SIGUSR1};
use signal_hook_tokio::Signals;
use snarkvm::{console::account::address::Address, prelude::Testnet3};
use tokio::sync::mpsc::Sender;
use tracing::{debug, error, info, warn};
use tracing_log::{log, LogTracer};
use tracing_subscriber::{layer::SubscriberExt, EnvFilter};

use crate::validator_peer::Node;
use crate::{
    accounting::{Accounting, AccountingMessage},
    //    operator_peer::Node,
    server::{Server, ServerMessage},
};

#[derive(Debug, Parser)]
#[clap(name = "pool_server", about = "Aleo mining pool server")]
struct Opt {
    /// Validator node address
    #[clap(short, long)]
    validator: Option<String>,

    /// Mining pool address
    #[clap(short, long)]
    address: Address<Testnet3>,

    /// Port to listen for incoming provers
    #[clap(short, long)]
    port: u16,

    /// API port
    #[clap(short, long = "api-port")]
    api_port: u16,

    /// Enable debug logging
    #[clap(short, long)]
    debug: bool,

    /// Enable trace logging
    #[clap(short, long)]
    trace: bool,

    /// Output log to file
    #[clap(long)]
    log: Option<String>,
}

#[tokio::main]
async fn main() {
    dotenv::dotenv().ok();
    let opt = Opt::parse();
    let tracing_level = if opt.trace {
        tracing::Level::TRACE
    } else if opt.debug {
        tracing::Level::DEBUG
    } else {
        tracing::Level::INFO
    };
    let _ = LogTracer::init_with_filter(log::LevelFilter::Info);
    let filter = EnvFilter::from_default_env()
        .add_directive(tracing_level.into())
        .add_directive("hyper=info".parse().unwrap())
        .add_directive("warp=info".parse().unwrap())
        .add_directive("warp=warn".parse().unwrap())
        .add_directive("tokio_util=info".parse().unwrap())
        .add_directive("api".parse().unwrap());
    let subscriber = tracing_subscriber::fmt::Subscriber::builder()
        .with_env_filter(filter)
        .finish();
    // .with(
    //     tracing_subscriber::fmt::Layer::default()
    //         .with_ansi(true)
    //         .with_writer(std::io::stdout),
    // );
    if let Some(log) = opt.log {
        let file = std::fs::File::create(log).unwrap();
        let file = tracing_subscriber::fmt::layer().with_writer(file).with_ansi(false);
        tracing::subscriber::set_global_default(subscriber.with(file))
            .expect("unable to set global default subscriber");
    } else {
        tracing::subscriber::set_global_default(subscriber).expect("unable to set global default subscriber");
    }

    rayon::ThreadPoolBuilder::new()
        .stack_size(8 * 1024 * 1024)
        .num_threads(num_cpus::get())
        .build_global()
        .unwrap();

    let validator = match opt.validator {
        Some(validator) => validator,
        None => {
            let bootstrap = [
                "164.92.111.59:4133",
                "159.223.204.96:4133",
                "167.71.219.176:4133",
                "157.245.205.209:4133",
                "134.122.95.106:4133",
                "161.35.24.55:4133",
                "138.68.103.139:4133",
                "207.154.215.49:4133",
                "46.101.114.158:4133",
                "138.197.190.94:4133",
            ];
            bootstrap.choose(&mut rand::thread_rng()).unwrap().to_string()
        }
    };
    let port = opt.port;

    let address = opt.address;

    let accounting = Accounting::init();

    let node = Node::init(validator);

    let server = Server::init(port, address, node.sender(), accounting.sender()).await;

    validator_peer::start(node, server.sender());

    api::start(opt.api_port, accounting.clone(), server.clone());

    match Signals::new([SIGABRT, SIGTERM, SIGHUP, SIGINT, SIGQUIT, SIGUSR1, SIGTSTP]) {
        Ok(signals) => {
            tokio::spawn(handle_signals(signals, accounting.clone(), server.sender()));
        }
        Err(err) => {
            error!("Unable to register signal handlers: {:?}", err);
            std::process::exit(1);
        }
    }

    std::future::pending::<()>().await;
}

async fn handle_signals(mut signals: Signals, accounting: Arc<Accounting>, server_sender: Sender<ServerMessage>) {
    while let Some(signal) = signals.next().await {
        info!("Received signal: {:?}", signal);
        let accounting_sender = accounting.sender();
        match signal {
            SIGABRT => {
                info!("Trying to salvage states before aborting...");
                let _ = accounting_sender.send(AccountingMessage::Exit).await;
                accounting.wait_for_exit().await;
                let _ = server_sender.send(ServerMessage::Exit).await;
                std::process::abort();
            }
            SIGTERM | SIGINT | SIGHUP | SIGQUIT => {
                info!("Saving states before exiting...");
                let _ = accounting_sender.send(AccountingMessage::Exit).await;
                accounting.wait_for_exit().await;
                let _ = server_sender.send(ServerMessage::Exit).await;
                std::process::exit(0);
            }
            SIGUSR1 => {
                debug!("Should do something useful here...");
            }
            SIGTSTP => {
                warn!("Suspending is not supported");
            }
            _ => unreachable!(),
        }
    }
}
