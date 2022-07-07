use crate::message::StratumMessage;
use bytes::BytesMut;
use erased_serde::Serialize as ErasedSerialize;
use json_rpc_types::{Id, Request, Response, Version};
use serde::{ser::SerializeSeq, Deserialize, Serialize};
use serde_json::Value;
use std::io;
use tokio_util::codec::{AnyDelimiterCodec, Decoder, Encoder};

pub struct StratumCodec {
    codec: AnyDelimiterCodec,
}

impl Default for StratumCodec {
    fn default() -> Self {
        Self {
            // Notify is ~400 bytes and submitt is about ~1750 bytes. 4096 should be enough for all messages.
            codec: AnyDelimiterCodec::new_with_max_length(vec![b'\n'], vec![b'\n'], 4096),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct NotifyParams(String, String, String, String, String, String, bool);

pub enum ResponseParams {
    Bool(bool),
    Array(Vec<Box<dyn ErasedSerialize + Send + Sync>>),
    Null,
}

impl Serialize for ResponseParams {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            ResponseParams::Bool(b) => serializer.serialize_bool(*b),
            ResponseParams::Array(v) => {
                let mut seq = serializer.serialize_seq(Some(v.len()))?;
                for item in v {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            ResponseParams::Null => serializer.serialize_none(),
        }
    }
}

impl<'de> Deserialize<'de> for ResponseParams {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        match value {
            Value::Bool(b) => Ok(ResponseParams::Bool(b)),
            Value::Array(a) => {
                let mut vec: Vec<Box<dyn ErasedSerialize + Send + Sync>> = Vec::new();
                let _ = a.iter().map(|v| match v {
                    Value::String(s) => vec.push(Box::new(s.clone())),
                    Value::Number(n) => vec.push(Box::new(n.as_u64())),
                    _ => {}
                });
                Ok(ResponseParams::Array(vec))
            }
            Value::Null => Ok(ResponseParams::Null),
            _ => Err(serde::de::Error::custom("invalid response params")),
        }
    }
}

impl Encoder<StratumMessage> for StratumCodec {
    type Error = io::Error;

    fn encode(&mut self, item: StratumMessage, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let bytes = match item {
            StratumMessage::Subscribe(id, user_agent, protocol_version) => {
                let request = Request {
                    jsonrpc: Version::V2,
                    method: "mining.subscribe",
                    params: Some(vec![user_agent, protocol_version]),
                    id: Some(id),
                };
                serde_json::to_vec(&request).unwrap_or_default()
            }
            StratumMessage::Authorize(id, worker_name, worker_password) => {
                let request = Request {
                    jsonrpc: Version::V2,
                    method: "mining.authorize",
                    params: Some(vec![worker_name, worker_password]),
                    id: Some(id),
                };
                serde_json::to_vec(&request).unwrap_or_default()
            }
            StratumMessage::SetTarget(difficulty_target) => {
                let request = Request {
                    jsonrpc: Version::V2,
                    method: "mining.set_target",
                    params: Some(vec![difficulty_target]),
                    id: None,
                };
                serde_json::to_vec(&request).unwrap_or_default()
            }
            StratumMessage::Notify(
                job_id,
                block_header_root,
                hashed_leaves_1,
                hashed_leaves_2,
                hashed_leaves_3,
                hashed_leaves_4,
                clean_jobs,
            ) => {
                let request = Request {
                    jsonrpc: Version::V2,
                    method: "mining.notify",
                    params: Some(NotifyParams(
                        job_id,
                        block_header_root,
                        hashed_leaves_1,
                        hashed_leaves_2,
                        hashed_leaves_3,
                        hashed_leaves_4,
                        clean_jobs,
                    )),
                    id: None,
                };
                serde_json::to_vec(&request).unwrap_or_default()
            }
            StratumMessage::Submit(id, worker_name, job_id, nonce, proof) => {
                let request = Request {
                    jsonrpc: Version::V2,
                    method: "mining.submit",
                    params: Some(vec![worker_name, job_id, nonce, proof]),
                    id: Some(id),
                };
                serde_json::to_vec(&request).unwrap_or_default()
            }
            StratumMessage::Response(id, result, error) => match error {
                Some(error) => {
                    let response = Response::<(), ()>::error(Version::V2, error, Some(id));
                    serde_json::to_vec(&response).unwrap_or_default()
                }
                None => {
                    let response = Response::<Option<ResponseParams>, ()>::result(Version::V2, result, Some(id));
                    serde_json::to_vec(&response).unwrap_or_default()
                }
            },
        };
        let string =
            std::str::from_utf8(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        self.codec
            .encode(string, dst)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        Ok(())
    }
}

impl Decoder for StratumCodec {
    type Item = StratumMessage;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        let string = self
            .codec
            .decode(src)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        if string.is_none() {
            return Ok(None);
        }
        let bytes = string.unwrap();
        let json = serde_json::from_slice::<serde_json::Value>(&bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        if !json.is_object() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Not an object"));
        }
        let object = json.as_object().unwrap();
        let result = if object.contains_key("method") {
            let request = serde_json::from_value::<Request<Vec<Value>>>(json)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            let id = request.id;
            let method = request.method.as_str();
            let params = request.params.unwrap_or_default();
            match method {
                "mining.subscribe" => {
                    if params.len() != 2 {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid params"));
                    }
                    let user_agent = params[0].as_str().unwrap_or_default();
                    let protocol_version = params[1].as_str().unwrap_or_default();
                    StratumMessage::Subscribe(
                        id.unwrap_or(Id::Num(0)),
                        user_agent.to_string(),
                        protocol_version.to_string(),
                    )
                }
                "mining.authorize" => {
                    if params.len() != 2 {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid params"));
                    }
                    let worker_name = params[0].as_str().unwrap_or_default();
                    let worker_password = params[1].as_str().unwrap_or_default();
                    StratumMessage::Authorize(
                        id.unwrap_or(Id::Num(0)),
                        worker_name.to_string(),
                        worker_password.to_string(),
                    )
                }
                "mining.set_target" => {
                    if params.len() != 1 {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid params"));
                    }
                    let difficulty_target = params[0].as_u64().unwrap_or_default();
                    StratumMessage::SetTarget(difficulty_target)
                }
                "mining.notify" => {
                    if params.len() != 7 {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid params"));
                    }
                    let job_id = params[0].as_str().unwrap_or_default();
                    let block_header_root = params[1].as_str().unwrap_or_default();
                    let hashed_leaves_1 = params[2].as_str().unwrap_or_default();
                    let hashed_leaves_2 = params[3].as_str().unwrap_or_default();
                    let hashed_leaves_3 = params[4].as_str().unwrap_or_default();
                    let hashed_leaves_4 = params[5].as_str().unwrap_or_default();
                    let clean_jobs = params[6].as_bool().unwrap_or(true);
                    StratumMessage::Notify(
                        job_id.to_string(),
                        block_header_root.to_string(),
                        hashed_leaves_1.to_string(),
                        hashed_leaves_2.to_string(),
                        hashed_leaves_3.to_string(),
                        hashed_leaves_4.to_string(),
                        clean_jobs,
                    )
                }
                "mining.submit" => {
                    if params.len() != 4 {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid params"));
                    }
                    let worker_name = params[0].as_str().unwrap_or_default();
                    let job_id = params[1].as_str().unwrap_or_default();
                    let nonce = params[2].as_str().unwrap_or_default();
                    let proof = params[3].as_str().unwrap_or_default();
                    StratumMessage::Submit(
                        id.unwrap_or(Id::Num(0)),
                        worker_name.to_string(),
                        job_id.to_string(),
                        nonce.to_string(),
                        proof.to_string(),
                    )
                }
                _ => {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "Unknown method"));
                }
            }
        } else {
            let response = serde_json::from_value::<Response<ResponseParams, ()>>(json)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            let id = response.id;
            match response.payload {
                Ok(payload) => StratumMessage::Response(id.unwrap_or(Id::Num(0)), Some(payload), None),
                Err(error) => StratumMessage::Response(id.unwrap_or(Id::Num(0)), None, Some(error)),
            }
        };
        Ok(Some(result))
    }
}
