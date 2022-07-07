use crate::codec::ResponseParams;
use json_rpc_types::{Error, Id};

pub enum StratumMessage {
    /// This first version doesn't support vhosts or session preserving.
    /// (id, user_agent, protocol_version)
    Subscribe(Id, String, String),

    /// (id, worker_name, worker_password)
    Authorize(Id, String, String),

    /// This is the difficulty target for the next job.
    /// (difficulty_target)
    SetTarget(u64),

    /// New job from the mining pool.
    /// See protocol specification for details about the fields.
    /// (job_id, block_header_root, hashed_leaves_1, hashed_leaves_2, hashed_leaves_3,
    ///  hashed_leaves_4, clean_jobs)
    Notify(String, String, String, String, String, String, bool),

    /// Submit shares to the pool.
    /// See protocol specification for details about the fields.
    /// (id, worker_name, job_id, nonce, proof)
    Submit(Id, String, String, String, String),

    /// (id, result, error)
    Response(Id, Option<ResponseParams>, Option<Error<()>>),
}

impl StratumMessage {
    pub fn name(&self) -> &'static str {
        match self {
            StratumMessage::Subscribe(_, _, _) => "mining.subscribe",
            StratumMessage::Authorize(_, _, _) => "mining.authorize",
            StratumMessage::SetTarget(_) => "mining.set_target",
            StratumMessage::Notify(_, _, _, _, _, _, _) => "mining.notify",
            StratumMessage::Submit(_, _, _, _, _) => "mining.submit",
            StratumMessage::Response(_, _, _) => "mining.response",
        }
    }
}
