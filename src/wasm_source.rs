use std::sync::mpsc;

use rustradio::Result;
use rustradio::block::{Block, BlockRet};
use rustradio::stream::{ReadStream, WriteStream, new_stream};

pub enum Msg {
    Eof,
    Extend(Vec<u8>),
}

#[derive(rustradio_macros::Block)]
pub struct WasmSource {
    buf: Vec<u8>,
    eof: bool,
    rx: mpsc::Receiver<Msg>,
    #[rustradio(out)]
    dst: WriteStream<u8>,
}

impl WasmSource {
    pub fn new() -> (Self, ReadStream<u8>, mpsc::Sender<Msg>) {
        let (tx, rx) = mpsc::channel();
        let (dst, src) = new_stream();
        (
            Self {
                buf: vec![],
                dst,
                eof: false,
                rx,
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
    fn check_msgs(&mut self) {
        loop {
            match self.rx.try_recv() {
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break,
                Ok(Msg::Eof) => self.set_eof(),
                Ok(Msg::Extend(v)) => self.extend(&v),
            }
        }
    }
}

impl Block for WasmSource {
    fn work(&mut self) -> Result<BlockRet<'_>> {
        loop {
            self.check_msgs();
            if self.buf.is_empty() {
                if self.eof {
                    return Ok(BlockRet::EOF);
                } else {
                    return Ok(BlockRet::Pending);
                }
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
