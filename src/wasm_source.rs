//! Block for getting data from the UI thread, which in turn is getting it from
//! websocket or a file.
use rustradio::block::{Block, BlockRet};
use rustradio::stream::{ReadStream, WriteStream, new_stream};
use rustradio::{Error, Result};

use crate::{ReqData, WorkerToMain, worker::post_message};

// TODO: magic value.
const PRODUCE_CHANNEL_SIZE: usize = 10;
const CHUNK_SIZE: u64 = 65536;

pub enum Msg {
    Eof,
    Extend(Vec<u8>),
}

/// Block for getting data from the UI thread, which in turn gets it from
/// websocket (data stream) or a file.
#[derive(rustradio_macros::Block)]
pub struct WasmSource {
    buf: Vec<u8>,
    eof: bool,
    pos: u64,
    outstanding_req: bool,
    rx: async_channel::Receiver<Msg>,
    #[rustradio(out)]
    dst: WriteStream<u8>,
}

impl WasmSource {
    pub fn new() -> (Self, ReadStream<u8>, async_channel::Sender<Msg>) {
        let (tx, rx) = async_channel::bounded(PRODUCE_CHANNEL_SIZE);
        let (dst, src) = new_stream();
        (
            Self {
                buf: vec![],
                dst,
                eof: false,
                rx,
                outstanding_req: false,
                pos: 0,
            },
            src,
            tx,
        )
    }
    fn set_eof(&mut self) {
        self.eof = true;
    }
    fn extend(&mut self, data: &[u8]) {
        self.buf.extend(data);
    }
    fn req_more(&mut self) -> Result<()> {
        if !self.outstanding_req {
            post_message(&WorkerToMain::ReqData(ReqData {
                receiver: crate::RECEIVER_SOURCE,
                pos: self.pos,
                size: CHUNK_SIZE,
            }))
            .map_err(|e| Error::msg(format!("{e:?}")))?;
            self.outstanding_req = true;
        }
        Ok(())
    }
    fn check_msgs(&mut self) {
        loop {
            #[allow(clippy::match_same_arms)]
            match self.rx.try_recv() {
                Err(async_channel::TryRecvError::Empty) => break,
                Err(async_channel::TryRecvError::Closed) => break,
                Ok(Msg::Eof) => {
                    self.set_eof();
                    self.outstanding_req = false;
                }
                Ok(Msg::Extend(v)) => {
                    self.pos += v.len() as u64;
                    self.extend(&v);
                    self.outstanding_req = false;
                }
            }
        }
    }
}

impl Block for WasmSource {
    fn work(&mut self) -> Result<BlockRet<'_>> {
        loop {
            self.check_msgs();
            log::trace!("WasmSource: buf len is {}", self.buf.len());
            if self.buf.is_empty() {
                if self.eof {
                    return Ok(BlockRet::EOF);
                }
                self.req_more()?;
                return Ok(BlockRet::Pending);
            }
            let mut o = self.dst.write_buf()?;
            if o.is_empty() {
                return Ok(BlockRet::WaitForStream(&self.dst, 1));
            }
            let n = self.buf.len().min(o.len());
            o.slice()[..n].copy_from_slice(&self.buf[..n]);
            o.produce(n, &[]);
            self.buf.drain(0..n);
        }
    }
}
