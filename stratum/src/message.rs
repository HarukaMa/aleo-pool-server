use json_rpc_types::{Error, Id};

use crate::codec::ResponseParams;

pub enum StratumMessage {
    /// This first version doesn't support vhosts.
    /// (id, user_agent, protocol_version, session_id)
    Subscribe(Id, String, String, Option<String>),

    /// (id, worker_name, worker_password)
    Authorize(Id, String, String),

    /// This is the difficulty target for the next job.
    /// (difficulty_target)
    SetTarget(u64),

    /// New job from the mining pool.
    /// See protocol specification for details about the fields.
    /// (job_id, epoch_challenge, address, clean_jobs)
    Notify(String, String, Option<String>, bool),

    /// Submit shares to the pool.
    /// See protocol specification for details about the fields.
    /// (id, worker_name, job_id, nonce, commitment, proof)
    Submit(Id, String, String, String, String, String),

    /// (id, result, error)
    Response(Id, Option<ResponseParams>, Option<Error<()>>),
}

impl StratumMessage {
    pub fn name(&self) -> &'static str {
        match self {
            StratumMessage::Subscribe(..) => "mining.subscribe",
            StratumMessage::Authorize(..) => "mining.authorize",
            StratumMessage::SetTarget(..) => "mining.set_target",
            StratumMessage::Notify(..) => "mining.notify",
            StratumMessage::Submit(..) => "mining.submit",
            StratumMessage::Response(..) => "mining.response",
        }
    }
}
