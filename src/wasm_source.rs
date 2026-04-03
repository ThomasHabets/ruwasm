use rustradio::Result;
use rustradio::block::{Block, BlockRet};
use rustradio::stream::{ReadStream, WriteStream, new_stream};

#[derive(rustradio_macros::Block)]
pub struct WasmSource {
    buf: Vec<u8>,
    eof: bool,
    #[rustradio(out)]
    dst: WriteStream<u8>,
}

impl WasmSource {
    pub fn new() -> (Self, ReadStream<u8>) {
        let (dst, src) = new_stream();
        (
            Self {
                buf: vec![],
                dst,
                eof: false,
            },
            src,
        )
    }
    pub fn set_eof(&mut self) {
        self.eof = true;
    }
    pub fn extend(&mut self, data: &[u8]) {
        self.buf.extend(data);
    }
}

impl Block for WasmSource {
    fn work(&mut self) -> Result<BlockRet<'_>> {
        loop {
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
