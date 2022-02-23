mod accounting;
mod api;
mod cache;
mod connection;
mod db;
mod message;
mod operator_peer;
mod server;

use std::sync::Arc;

use futures::stream::StreamExt;
use signal_hook::consts::{SIGABRT, SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGTSTP, SIGUSR1};
use signal_hook_tokio::Signals;
use structopt::StructOpt;
use tokio::sync::mpsc::Sender;
use tracing::{debug, error, info, warn};
use tracing_log::{log, LogTracer};
use tracing_subscriber::{layer::SubscriberExt, EnvFilter};

use crate::{
    accounting::{Accounting, AccountingMessage},
    operator_peer::Node,
    server::{Server, ServerMessage},
};

#[derive(Debug, StructOpt)]
#[structopt(name = "pool_server", about = "Aleo mining pool server", setting = structopt::clap::AppSettings::ColoredHelp)]
struct Opt {
    /// Full operator node address
    #[structopt(short = "o", long = "operator")]
    operator: String,

    /// Port to listen for incoming provers
    #[structopt(short = "p", long = "port")]
    port: u16,

    /// API port
    #[structopt(short = "a", long = "api-port")]
    api_port: u16,

    /// Enable debug logging
    #[structopt(short = "d", long = "debug")]
    debug: bool,

    /// Enable trace logging
    #[structopt(long = "trace")]
    trace: bool,

    /// Output log to file
    #[structopt(long = "log")]
    log: Option<String>,
}

#[tokio::main]
async fn main() {
    let opt = Opt::from_args();
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

    let operator = opt.operator;
    let port = opt.port;

    let accounting = Accounting::init(operator.clone());

    let node = Node::init(operator);

    let server = Server::init(port, node.sender(), accounting.sender()).await;

    operator_peer::start(node, server.sender());

    api::start(opt.api_port, accounting.clone(), server.clone());

    match Signals::new(&[SIGABRT, SIGTERM, SIGHUP, SIGINT, SIGQUIT, SIGUSR1, SIGTSTP]) {
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
