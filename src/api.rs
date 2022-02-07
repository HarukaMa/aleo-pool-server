use std::{convert::Infallible, net::SocketAddr, sync::Arc};

use serde_json::json;
use tokio::task;
use tracing::info;
use warp::{
    addr::remote,
    get,
    head,
    path,
    reply,
    reply::{json, Json},
    serve,
    Filter,
    Reply,
};

use crate::{Accounting, Server};

pub fn start(port: u16, accounting: Arc<Accounting>, server: Arc<Server>) {
    task::spawn(async move {
        let current_round = path("current_round")
            .and(use_accounting(accounting.clone()))
            .then(current_round)
            .boxed();
        let admin_current_round = path!("admin" / "current_round")
            .and(remote())
            .and(use_accounting(accounting.clone()))
            .then(admin_current_round)
            .boxed();

        let pool_stats = path("stats").and(use_server(server.clone())).then(pool_stats).boxed();

        let endpoints = pool_stats.or(current_round).or(admin_current_round).boxed();

        let routes = get()
            .or(head())
            .unify()
            .and(endpoints)
            .with(warp::log("aleo_pool_server::api"));
        info!("Starting API server on port {}", port);
        serve(routes).run(([0, 0, 0, 0], port)).await;
    });
}

fn use_accounting(
    accounting: Arc<Accounting>,
) -> impl Filter<Extract = (Arc<Accounting>,), Error = Infallible> + Clone {
    warp::any().map(move || accounting.clone())
}
fn use_server(server: Arc<Server>) -> impl Filter<Extract = (Arc<Server>,), Error = Infallible> + Clone {
    warp::any().map(move || server.clone())
}

async fn pool_stats(server: Arc<Server>) -> Json {
    json(&json!({
        "online_addresses": server.online_addresses().await,
        "online_provers": server.online_provers().await,
        "speed": server.pool_speed().await,
    }))
}

async fn current_round(accounting: Arc<Accounting>) -> Json {
    let data = accounting.current_round().await;

    json(&json! ({
        "n": data["n"],
        "current_n": data["current_n"],
        "provers": data["provers"],
    }))
}

async fn admin_current_round(addr: Option<SocketAddr>, accounting: Arc<Accounting>) -> impl Reply {
    let addr = addr.unwrap();
    if addr.ip().is_loopback() {
        let pplns = accounting.current_round().await;

        Ok(reply::with_status(json(&pplns), warp::http::StatusCode::OK))
    } else {
        Ok(reply::with_status(
            json(&"Method Not Allowed"),
            warp::http::StatusCode::METHOD_NOT_ALLOWED,
        ))
    }
}
