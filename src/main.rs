mod operator_peer;
mod server;

use std::net::SocketAddr;
use structopt::StructOpt;
use tracing_subscriber::layer::SubscriberExt;
use crate::operator_peer::Node;
use crate::server::Server;

#[derive(Debug, StructOpt)]
#[structopt(name = "operator", about = "Aleo standalone operator", setting = structopt::clap::AppSettings::ColoredHelp)]
struct Opt {
    /// Full operator node address
    #[structopt(short = "o", long = "operator")]
    operator: String,

    /// Port to listen for incoming provers
    #[structopt(short = "p", long = "port")]
    port: u16,

    /// Enable debug logging
    #[structopt(short = "d", long = "debug")]
    debug: bool,

    /// Output log to file
    #[structopt(long = "log")]
    log: Option<String>,
}

#[tokio::main]
async fn main() {
    let opt = Opt::from_args();
    let tracing_level = if opt.debug {
        tracing::Level::DEBUG
    } else {
        tracing::Level::INFO
    };
    let subscriber = tracing_subscriber::fmt::Subscriber::builder()
        .with_max_level(tracing_level)
        .finish();
    // .with(
    //     tracing_subscriber::fmt::Layer::default()
    //         .with_ansi(true)
    //         .with_writer(std::io::stdout),
    // );
    if let Some(log) = opt.log {
        let file = std::fs::File::create(log).unwrap();
        let file = tracing_subscriber::fmt::layer()
            .with_writer(file)
            .with_ansi(false);
        tracing::subscriber::set_global_default(subscriber.with(file))
            .expect("unable to set global default subscriber");
    } else {
        tracing::subscriber::set_global_default(subscriber)
            .expect("unable to set global default subscriber");
    }

    let operator = opt.operator;
    let port = opt.port;
    let debug = opt.debug;

    let node = Node::init(operator);

    let server = Server::init(port, node.router().clone());

    crate::operator_peer::start(node.clone(), node.receiver());

    std::future::pending::<()>().await;

}
