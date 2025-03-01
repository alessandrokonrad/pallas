use std::fmt::Debug;

use itertools::Itertools;
use log::debug;
use pallas_codec::minicbor::{decode, encode, Decode, Decoder, Encode, Encoder};

use crate::machines::{Agent, MachineError, Transition};

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum State {
    Idle,
    TxIdsNonBlocking,
    TxIdsBlocking,
    Txs,
    Done,
}

pub type Blocking = bool;

pub type TxCount = u16;

pub type TxSizeInBytes = u32;

pub type TxId = u64;

#[derive(Debug)]
pub struct TxIdAndSize(TxId, TxSizeInBytes);

impl Encode<()> for TxIdAndSize {
    fn encode<W: encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut (),
    ) -> Result<(), encode::Error<W::Error>> {
        e.array(2)?;
        e.u64(self.0)?;
        e.u32(self.1)?;

        Ok(())
    }
}

impl<'b> Decode<'b, ()> for TxIdAndSize {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut ()) -> Result<Self, decode::Error> {
        d.array()?;
        let id = d.u64()?;
        let size = d.u32()?;

        Ok(Self(id, size))
    }
}

pub type TxBody = Vec<u8>;

#[derive(Debug, Clone)]
pub struct Tx(TxId, TxBody);

impl From<&Tx> for TxIdAndSize {
    fn from(other: &Tx) -> Self {
        TxIdAndSize(other.0, other.1.len() as u32)
    }
}

#[derive(Debug)]
pub enum Message {
    RequestTxIds(Blocking, TxCount, TxCount),
    ReplyTxIds(Vec<TxIdAndSize>),
    RequestTxs(Vec<TxId>),
    ReplyTxs(Vec<TxBody>),
    Done,
}

impl Encode<()> for Message {
    fn encode<W: encode::Write>(
        &self,
        e: &mut Encoder<W>,
        _ctx: &mut (),
    ) -> Result<(), encode::Error<W::Error>> {
        match self {
            Message::RequestTxIds(blocking, ack, req) => {
                e.array(4)?.u16(0)?;
                e.bool(*blocking)?;
                e.u16(*ack)?;
                e.u16(*req)?;
                Ok(())
            }
            Message::ReplyTxIds(ids) => {
                e.array(2)?.u16(1)?;
                e.array(ids.len() as u64)?;
                for id in ids {
                    e.encode(id)?;
                }
                Ok(())
            }
            Message::RequestTxs(ids) => {
                e.array(2)?.u16(2)?;
                e.array(ids.len() as u64)?;
                for id in ids {
                    e.u64(*id)?;
                }
                Ok(())
            }
            Message::ReplyTxs(txs) => {
                e.array(2)?.u16(3)?;
                e.array(txs.len() as u64)?;
                for tx in txs {
                    e.bytes(tx)?;
                }
                Ok(())
            }
            Message::Done => {
                e.array(1)?.u16(4)?;
                Ok(())
            }
        }
    }
}

impl<'b> Decode<'b, ()> for Message {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut ()) -> Result<Self, decode::Error> {
        d.array()?;
        let label = d.u16()?;

        match label {
            0 => {
                let blocking = d.bool()?;
                let ack = d.u16()?;
                let req = d.u16()?;
                Ok(Message::RequestTxIds(blocking, ack, req))
            }
            1 => {
                let items = d.decode()?;
                Ok(Message::ReplyTxIds(items))
            }
            2 => {
                let ids = d.array_iter::<TxId>()?.try_collect()?;
                Ok(Message::RequestTxs(ids))
            }
            3 => {
                todo!()
            }
            4 => Ok(Message::Done),
            _ => Err(decode::Error::message(
                "unknown variant for txsubmission message",
            )),
        }
    }
}

/// A very basic tx provider agent with a fixed set of tx to submit
///
/// This provider takes a set of tx from a vec as the single, static source of
/// data to transfer to the consumer. It's main use is for implementing peers
/// that need to answer to v1 implementations of the Tx-Submission
/// mini-protocol. Since v1 nodes dont' wait for a 'Hello' message, the peer
/// needs to be prepared to receive Tx requests. This naive provider serves as a
/// good placeholder for those scenarios.
#[derive(Debug)]
pub struct NaiveProvider {
    pub state: State,
    pub fifo_txs: Vec<Tx>,
    pub requested_ids_count: usize,
    pub requested_txs: Option<Vec<TxId>>,
}

impl NaiveProvider {
    pub fn initial(fifo_txs: Vec<Tx>) -> Self {
        Self {
            state: State::Idle,
            requested_ids_count: 0,
            requested_txs: None,
            fifo_txs,
        }
    }

    fn reply_tx_ids_msg(&self) -> Message {
        debug!(
            "sending next {} tx ids from fifo queue",
            self.requested_ids_count
        );

        let to_send = self.fifo_txs[0..self.requested_ids_count]
            .iter()
            .map_into()
            .collect_vec();

        Message::ReplyTxIds(to_send)
    }

    fn reply_txs_msg(&self) -> Message {
        let matches = self
            .fifo_txs
            .iter()
            .filter(|Tx(candidate_id, _)| match &self.requested_txs {
                Some(requested) => requested.iter().contains(candidate_id),
                None => false,
            })
            .map(|Tx(_, body)| body.clone())
            .collect_vec();

        Message::ReplyTxs(matches)
    }

    fn on_tx_ids_request(
        self,
        acknowledged_count: usize,
        requested_ids_count: usize,
    ) -> Transition<Self> {
        debug!(
            "new tx id request {} (ack: {})",
            requested_ids_count, acknowledged_count
        );

        debug!("draining {} from tx fifo queue", acknowledged_count);
        let new_fifo: Vec<_> = self
            .fifo_txs
            .into_iter()
            .skip(acknowledged_count - 1)
            .collect();

        Ok(Self {
            state: State::Idle,
            requested_ids_count,
            fifo_txs: new_fifo,
            ..self
        })
    }

    fn on_txs_request(self, requested_txs: Vec<TxId>) -> Transition<Self> {
        debug!("new txs request {:?}", requested_txs,);

        Ok(Self {
            state: State::Idle,
            requested_txs: Some(requested_txs),
            ..self
        })
    }
}

impl Agent for NaiveProvider {
    type Message = Message;
    type State = State;

    fn state(&self) -> &Self::State {
        &self.state
    }

    fn is_done(&self) -> bool {
        self.state == State::Done
    }

    fn has_agency(&self) -> bool {
        match self.state {
            State::Idle => false,
            State::TxIdsNonBlocking => true,
            State::TxIdsBlocking => true,
            State::Txs => true,
            State::Done => false,
        }
    }

    fn build_next(&self) -> Self::Message {
        match &self.state {
            State::TxIdsNonBlocking => self.reply_tx_ids_msg(),
            State::TxIdsBlocking => Message::Done,
            State::Txs => self.reply_txs_msg(),
            _ => panic!(""),
        }
    }

    fn apply_start(self) -> Transition<Self> {
        Ok(self)
    }

    fn apply_outbound(self, msg: Self::Message) -> Transition<Self> {
        match (self.state, msg) {
            (State::TxIdsNonBlocking, Message::ReplyTxIds(_)) => Ok(Self {
                state: State::Idle,
                ..self
            }),
            (State::TxIdsBlocking, Message::Done) => Ok(Self {
                state: State::Done,
                ..self
            }),
            (State::Txs, Message::ReplyTxs(_)) => Ok(Self {
                state: State::Idle,
                ..self
            }),
            _ => panic!(),
        }
    }

    fn apply_inbound(self, msg: Self::Message) -> Transition<Self> {
        match (&self.state, msg) {
            (State::Idle, Message::RequestTxIds(block, ack, req)) if !block => {
                self.on_tx_ids_request(ack as usize, req as usize)
            }
            (State::Idle, Message::RequestTxIds(block, _, _)) if block => Ok(Self {
                state: State::TxIdsBlocking,
                ..self
            }),
            (State::Idle, Message::RequestTxs(ids)) => self.on_txs_request(ids),
            (state, msg) => Err(MachineError::invalid_msg::<Self>(state, &msg)),
        }
    }
}
